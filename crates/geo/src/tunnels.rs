//! Roads that run below the ground: the walls, the roofs and the hole you get in
//! by.
//!
//! No Bevy: street runs and their heights in, [`SurfaceMesh`]es out. The
//! streaming half is `systems::tunnels`, and `docs/notes/tunnels.md` and
//! `docs/notes/bridge-heights.md` have the reasoning this module only summarises.
//!
//! # A road that is below its land
//!
//! A bridge and a tunnel are one road at two levels, and this module is the
//! second half of that sentence. Every street has a height solved on the road
//! network (`crate::road_levels`), and around a structure also the level of the
//! **land** it runs through. Where the two part, something has to be built:
//!
//! - **above** the land the road is carried — a deck, `vector_tiles`' business;
//! - **below** it, the road is **walled**: a retaining wall either side from the
//!   carriageway up to the land, and the ground over it taken away
//!   ([`TunnelTessellation::openings`]) — a cutting;
//! - and where the tiles say the road is *under* the ground (`tunnel`), and a
//!   roof fits between its headroom and the land, it is also **roofed** — a
//!   tunnel, with a headwall over each mouth where the roof stops.
//!
//! Which stretches are structures, and how deep they run, is not decided here. It
//! is `road_levels::structures`, on the solved heights — from the survey where
//! atlas measured one, from the tiles alone ([`unmeasured_profiles`]) where it did
//! not — and it arrives as the land height at each point of a street run that is
//! part of one. This module turns that into a shape and asks nothing else. So the
//! ramp down to a portal, the cutting between two lids and a ramp that crosses a
//! tile border are not three cases with three planners: they are points of a run
//! whose road is below its land.
//!
//! # The tiles carry a boolean and nothing else
//!
//! Not a depth, and — measured on the Groningen fixture, which is what
//! [`tests::the_tiles_say_which_streets_are_tunnels`] pins — **not a `layer`
//! either**. So with nothing measured a tunnel runs [`LEVEL_DEPTH_M`] under the
//! land, a convention rather than a measurement, and it is the one number a real
//! source replaces: atlas's solve keeps the road under the lid by that much and
//! otherwise goes where the survey says.
//!
//! # An arcade is flagged a tunnel too, so ask the buildings
//!
//! OSM's `tunnel` also covers `building_passage` — a way through a building, at
//! ground level, that people walk down — and roughly a quarter of the flagged ways
//! around Groningen's centre are those. [`building_passages`] tests each flagged
//! way against the tile's building polygons, and what comes back is an ordinary
//! street at ground level (`road_levels::street_runs` gives it level 0).
//!
//! # Heights are in the caller's frame, and final
//!
//! The road and land heights handed to [`build_structures`] are where the mesh is
//! drawn: the client passes NAP less its field datum, and nothing lifts the
//! result afterwards. Every earlier version of this module drew at an invented
//! depth and then lifted it back by whatever the survey had "already dug" — and
//! each consumer that needed the road's height (the floor, the traffic, the fill
//! over a lid, the shot metrics) had to repeat that arithmetic and agree. They
//! did not always. A height that is final needs no agreement.

use crate::road_levels::{self, Height, StreetRun};
use crate::road_texture::RoadClass;
use crate::terrain_refine::GroundField;
use crate::vector_tiles::{
    clip_polyline, flag_of, road_depth_rung, SurfaceGroup, SurfaceMesh, TileData, VERTEX_BYTES,
};
use geo_types::Geometry;
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay::{Overlay, ShapeType};
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;
use mvt_reader::feature::Feature;

/// How far a tunnel runs under its land with nothing measured, metres, to the
/// carriageway: 4.8 m of headroom, a slab and the make-up under the deck. A
/// convention, not a measurement — see the module note.
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

/// [`LEVEL_DEPTH_M`] for a tunnel carrying a path. A cycle underpass is not a road
/// tunnel with smaller cars in it: 2.5 m of headroom against 4.8, a thinner deck,
/// and — measured from the other end — ramps that typically drop about 3.5 m.
/// Given the road's seven metres a 30 m underpass would ramp for 70 m each side.
pub const PATH_DEPTH_M: f32 = 3.6;

/// How far under the land a roof must stay to be a roof, metres (negative): a
/// ceiling flush with the ground is coplanar with the ground sheet over it, and
/// six hundred millimetres is roughly a real slab.
pub const CEILING_MAX_Y: f32 = -0.6;

/// How far below its land a stretch must run before the ground beside it is put
/// back to the land ([`Tunnel::fill_weight`]): the same metre at which a road
/// stops being laid in a bank and starts being walled (`road_levels::SUNK_M`).
pub const FILL_DEPTH_M: f32 = road_levels::SUNK_M;

/// How far out the land around a structure is read, and how far the ground beside
/// one tapers back to what was measured, metres.
///
/// Wider than the cut and narrower than the block: a dual carriageway in a trench
/// is about 25 m of road plus its walls, so a shorter reach samples the trench
/// itself and finds no drop at all, while a much longer one starts finding the
/// next street's embankment.
pub const DATUM_M: f32 = 30.0;

/// A tunnel's depth under its land with nothing measured.
#[must_use]
pub fn level_depth(class: RoadClass) -> f32 {
    if matches!(class, RoadClass::Path) {
        PATH_DEPTH_M
    } else {
        LEVEL_DEPTH_M
    }
}

/// The headroom a tunnel carrying this class keeps.
#[must_use]
pub fn clearance(class: RoadClass) -> f32 {
    if matches!(class, RoadClass::Path) {
        PATH_CLEARANCE_M
    } else {
        CLEARANCE_M
    }
}

/// One structure's plan corridor and its heights along it, for the consumers
/// that need to know where the road *is* rather than what it looks like: the
/// avatar's footing, the fill over a lid, and the ground cut at an opening.
#[derive(Clone, Debug, Default)]
pub struct Tunnel {
    /// Centre line, tile-local metres.
    pub centre: Vec<[f32; 2]>,
    /// The carriageway's height at each centre vertex, in the frame the meshes
    /// are drawn in.
    pub road_y: Vec<f32>,
    /// The land's height at each centre vertex, same frame: what the walls of an
    /// open stretch reach up to, and what the ground over a roofed one is.
    pub land_y: Vec<f32>,
    /// Half the carriageway width, metres.
    pub half_w: f32,
    /// Headroom over the deck, metres. Carried rather than read off
    /// [`CLEARANCE_M`] so a consumer asking "am I inside this tunnel" gets the
    /// answer for *this* tunnel.
    pub clearance: f32,
    /// Whether each centre vertex has a roof over it, as against being open.
    ///
    /// Carried because the *ground* needs it. Where a tunnel has a lid, the land
    /// above it is continuous — you walk over the top — and the measured height
    /// data does not always agree: Groningen's ring was flown as an open trench
    /// and has since been rebuilt with a lid, so the survey digs nine metres out
    /// of ground that is now whole.
    pub covered: Vec<bool>,
}

/// The ceiling over a roofed tunnel's road at `road` under land at `land`: its
/// headroom over the road, held [`ROOF_SLAB_M`] under the land where the two
/// do not both fit — which only a street crossing over it roofs
/// ([`crossed_by_streets`]).
#[must_use]
pub fn ceiling_over(road: f32, land: f32, clearance: f32) -> f32 {
    (road + clearance).min(land - ROOF_SLAB_M)
}

impl Tunnel {
    /// The ceiling at centre vertex `k` ([`ceiling_over`]).
    #[must_use]
    pub fn ceiling(&self, k: usize) -> f32 {
        ceiling_over(self.road_y[k], self.land_y[k], self.clearance)
    }

    /// The carriageway's height under `p`, if `p` is over this corridor.
    ///
    /// `None` rather than a default for "not over it": a caller that cannot tell
    /// those apart puts the avatar on a tunnel floor in the middle of a field.
    #[must_use]
    pub fn road_under(&self, p: [f32; 2]) -> Option<f32> {
        along_line(&self.centre, &self.road_y, self.half_w, p)
    }

    /// The carriageway's height under `p` on this corridor widened by `reach`
    /// metres either side, or `None` past that.
    #[must_use]
    pub fn road_near(&self, p: [f32; 2], reach: f32) -> Option<f32> {
        along_line(&self.centre, &self.road_y, self.half_w + reach, p)
    }

    /// How far the road runs under its land at `p` (negative), on this corridor
    /// widened by `reach` metres either side, or `None` past that.
    #[must_use]
    pub fn depth_near(&self, p: [f32; 2], reach: f32) -> Option<f32> {
        let half = self.half_w + reach;
        Some(
            along_line(&self.centre, &self.road_y, half, p)?
                - along_line(&self.centre, &self.land_y, half, p)?,
        )
    }

    /// How far `p` is from this tunnel's carriageway in plan, and how far the
    /// road there runs under its land (negative). Zero distance means `p` is over
    /// it.
    ///
    /// **This is the question a picture of a tunnel cannot answer.** A road seen
    /// from a low oblique is foreshortened into a band a few pixels deep, and a
    /// band seven metres down looks very much like a band on the ground —
    /// especially beside a second, ordinary road running parallel, which is
    /// exactly what a tunnel approach is. The distance separates them, and unlike
    /// [`Self::road_under`] it answers for a point that is *not* over the tunnel.
    #[must_use]
    pub fn distance_to(&self, p: [f32; 2]) -> (f32, f32) {
        let mut best = (f32::MAX, 0.0);
        for i in 0..self.centre.len().saturating_sub(1) {
            let Some((d, t)) = project(self.centre[i], self.centre[i + 1], p) else {
                continue;
            };
            if d < best.0 {
                let lerp = |v: &[f32]| v[i] + (v[i + 1] - v[i]) * t;
                best = (d, lerp(&self.road_y) - lerp(&self.land_y));
            }
        }
        ((best.0 - self.half_w).max(0.0), best.1)
    }

    /// The land's height nearest `p` on this corridor, and how far `p` is from
    /// the carriageway in plan: what the ground at `p` is put back to where it is
    /// filled ([`Self::fill_weight`]).
    #[must_use]
    pub fn land_near(&self, p: [f32; 2]) -> (f32, f32) {
        let mut best = (f32::MAX, 0.0);
        for i in 0..self.centre.len().saturating_sub(1) {
            let Some((d, t)) = project(self.centre[i], self.centre[i + 1], p) else {
                continue;
            };
            if d < best.0 {
                best = (
                    d,
                    self.land_y[i] + (self.land_y[i + 1] - self.land_y[i]) * t,
                );
            }
        }
        ((best.0 - self.half_w).max(0.0), best.1)
    }

    /// Bounding box of the covered span, widened by the carriageway's half width:
    /// `[min_x, min_y, max_x, max_y]`, or `None` where nothing is covered.
    ///
    /// [`covered_weight`](Self::covered_weight) walks every segment and its only
    /// early exit fires *inside* a tunnel, so a point nowhere near one — which is
    /// almost every point the ground asks about — is its worst case. A caller
    /// holding many tunnels should reject against this first, expanded by its own
    /// taper.
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

    /// Whether segment `i` is covered, or sunk deep enough under its land that
    /// the ground beside it is put back to the land.
    fn fills(&self, i: usize) -> bool {
        (self.covered[i] && self.covered[i + 1])
            || (self.road_y[i] - self.land_y[i]).min(self.road_y[i + 1] - self.land_y[i + 1])
                < -FILL_DEPTH_M
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

    /// How much the land at `p` should be treated as continuous ground over a
    /// lid: 1 directly over a covered stretch, falling to 0 `taper_m` beyond it.
    #[must_use]
    pub fn covered_weight(&self, p: [f32; 2], taper_m: f32) -> f32 {
        self.weight_where(p, taper_m, |i| self.covered[i] && self.covered[i + 1])
    }

    /// How much the ground at `p` should be put back to the land
    /// ([`Self::land_near`]): 1 over a covered stretch or one sunk more than
    /// [`FILL_DEPTH_M`], falling smoothly to 0 `taper_m` beyond it.
    ///
    /// **Two sources disagree and one of them is stale.** The height survey
    /// measures what it flew over, and Groningen's ring was an open trench when it
    /// was flown and has a lid on it now — so the survey digs nine metres out of
    /// ground that is whole, and beside the open cuttings it still shows that
    /// trench's sloped sides wider than the cutting's walls. The map's tag is the
    /// newer fact: the ground over a lid and beside a wall is the land.
    ///
    /// The taper is generous on purpose: a trench is wider than the road in it —
    /// two carriageways, their walls and their verges — so filling only the
    /// carriageway's own width would leave a ditch either side of a flush lid.
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
            let Some((d, _)) = project(self.centre[i], self.centre[i + 1], p) else {
                continue;
            };
            let d = d - self.half_w;
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

/// The distance from `p` to the segment `a b`, and how far along it the nearest
/// point is (0..1). `None` for a degenerate segment.
fn project(a: [f32; 2], b: [f32; 2], p: [f32; 2]) -> Option<(f32, f32)> {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len_sq = dx * dx + dy * dy;
    if len_sq <= 1e-9 {
        return None;
    }
    // Clamped projection: past either end of a segment the nearest point is that
    // end, which is what makes a chain of segments behave as one corridor with
    // rounded ends rather than as a set of infinite strips.
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len_sq).clamp(0.0, 1.0);
    Some(((p[0] - (a[0] + dx * t)).hypot(p[1] - (a[1] + dy * t)), t))
}

/// Everything one tile's structures produce.
#[derive(Default)]
pub struct TunnelTessellation {
    /// The drawn structures: carriageway, walls, roofs and headwalls.
    pub meshes: Vec<SurfaceMesh>,
    /// Plan corridors of the open stretches, tile-local metres, each a closed
    /// ring. **The caller must cut these out of the ground**, or the entrance is
    /// roofed by the ground sheet and the tunnel cannot be entered.
    pub openings: Vec<Vec<[f32; 2]>>,
    /// Every structure in the tile, for footing and collision.
    pub tunnels: Vec<Tunnel>,
    /// The same corridors keyed to the `streets` feature each came off, for the
    /// consumers that **travel** the road rather than draw it. See [`SunkenWay`].
    pub sunken: Vec<SunkenWay>,
}

/// One street's sunken profile, keyed to the feature it belongs to.
///
/// **Keyed, because position is not enough.** `DeckPlates::lift_at` sampled a
/// bridge's height as a function of world XZ and hoisted the underpass onto the
/// viaduct above it (`grade-separation.md`); asking "is there a tunnel under this
/// car" has the identical shape, and Groningen's ring has ordinary streets
/// crossing over it. The only thing that knows whether a car is *on* the sunken
/// road or on the one above it is which feature its line came from, so that is
/// what travels with the profile.
#[derive(Clone, Debug)]
pub struct SunkenWay {
    /// Index into the tile's `streets` features, as `tile.features` orders them.
    pub feature: usize,
    /// Centre line, tile-local metres.
    pub centre: Vec<[f32; 2]>,
    /// The carriageway's height at each vertex, in the meshes' frame.
    pub road_y: Vec<f32>,
    /// Half the carriageway, so a point beside the corridor can be told from one
    /// on it.
    pub half_w: f32,
}

impl SunkenWay {
    /// The carriageway's height under `p`, or `None` where `p` is not over this
    /// corridor — the same walk [`Tunnel::road_under`] does, so a car and the
    /// deck it is drawn on agree.
    #[must_use]
    pub fn road_under(&self, p: [f32; 2]) -> Option<f32> {
        along_line(&self.centre, &self.road_y, self.half_w, p)
    }
}

/// A value along a corridor under `p`, or `None` where `p` is not over it: the
/// nearest segment within `half_w`, interpolated.
fn along_line(centre: &[[f32; 2]], values: &[f32], half_w: f32, p: [f32; 2]) -> Option<f32> {
    let mut best: Option<(f32, f32)> = None;
    for i in 0..centre.len().saturating_sub(1) {
        let Some((d, t)) = project(centre[i], centre[i + 1], p) else {
            continue;
        };
        if d > half_w {
            continue;
        }
        let y = values[i] + (values[i + 1] - values[i]) * t;
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, y));
        }
    }
    best.map(|(_, y)| y)
}

/// The measured ground with the land put back over every lid and beside every
/// wall ([`Tunnel::fill_weight`]).
///
/// **It is a `GroundField` and not a pass over finished vertices**, and that is
/// the whole point of the type. Added afterwards, the lift is a second surface the
/// refinement never saw: a land parcel earcut into two triangles interpolates it
/// across a 100 m chord while the road beside it, which has a vertex every few
/// metres, follows it — so the two disagree by metres and the ground tears open
/// along the buried road. Handed to [`crate::terrain_refine::refine_for_terrain`]
/// as the crease field instead, the fill is part of the surface being resolved.
///
/// Where atlas graded the ground it already did this (`terrain_grade::fill_to_land`),
/// and doing it again changes nothing that matters: the ground there is the land.
pub struct Lidded<'a> {
    /// The measured ground, before any fill.
    pub ground: &'a dyn GroundField,
    /// The land a point is put back to.
    pub around: &'a dyn Fn(f32, f32) -> f32,
    /// [`Tunnel::fill_weight`] over whichever tunnels the caller holds.
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
        .any(|t| t.covered_weight(p, 0.0) >= 1.0 && t.depth_near(p, 0.0).is_some_and(|d| d < -2.0))
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
            let sunken = tunnels
                .iter()
                .any(|tu| tu.depth_near(p, WALL_REACH_M).is_some_and(|d| d < -1.0));
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

/// A surface street line: its geometry in tile-local metres, and
/// whether each end is the line's own (as against a cut by the tile border).
pub(crate) struct SurfaceLine {
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
    for feature in tile.features(meta) {
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

// ─── Building the structures ─────────────────────────────────────────────────

/// One street run's heights, for [`build_structures`]: the road at each of the
/// run's points, and the land at each point that is part of a structure — `NaN`
/// elsewhere. Both parallel to [`StreetRun::points`], in the frame the meshes are
/// drawn in.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub road: Vec<f32>,
    pub land: Vec<f32>,
    /// Where the walls stand at each point, left then right, as distances from
    /// the centre line: `0` where that side has no wall because the ground rises
    /// there as a bank, `NaN` where nothing was measured and the wall stands at
    /// the carriageway's edge. Empty for none measured at all.
    pub walls: Vec<[f32; 2]>,
}

/// One stretch of a run that is a structure, settled and ready to mesh.
#[derive(Clone, Debug)]
struct Plan {
    /// Centre line, tile-local metres.
    centre: Vec<[f32; 2]>,
    /// Distance along `centre` at each vertex — the texture's `v`, kept rather
    /// than re-derived so the deck and the walls cannot disagree about it.
    travelled: Vec<f32>,
    road_y: Vec<f32>,
    land_y: Vec<f32>,
    /// The measured walls at each vertex, as [`Profile::walls`] (`NaN` where
    /// none were).
    walls: Vec<[f32; 2]>,
    /// Whether another street crosses over each vertex ([`crossed_by_streets`]).
    crossed: Vec<bool>,
    /// The ceiling at each vertex: the headroom over the road, held
    /// [`ROOF_SLAB_M`] under the land where the two do not both fit.
    ceiling: Vec<f32>,
    /// Whether each vertex has a roof over it.
    roofed: Vec<bool>,
    class: RoadClass,
    half: f32,
    clearance: f32,
    /// The `streets` feature this stretch came off, so the drive network can key
    /// a car's height to its own road ([`SunkenWay`]).
    feature: usize,
    /// Whether each end was cut by the tile border: the structure carries on in
    /// the next tile, so a roof reaching it needs no headwall.
    cut: [bool; 2],
    /// Whether each end is a mouth — roofed here, and open to the sky beyond —
    /// and so needs a headwall.
    mouth: [bool; 2],
    /// Whether each end is its street's own end, as against a point where the
    /// street carries on as an ordinary road laid in the ground.
    run_end: [bool; 2],
    /// Whether each end is where its road ends in nothing: no other street in the
    /// tile comes within [`road_levels::TEE_M`] of it, and the border did not cut
    /// it. A tunnel into a garage the map does not draw ends that way, and so does
    /// a stub of path; below the land such an end is closed by a wall.
    dead: [bool; 2],
}

/// How far a simplified structure may stray from the run it came from, metres —
/// in plan and in height alike. Well under what any camera can see, and it takes
/// a straight run's vertices from one every three metres to one per bend.
const SIMPLIFY_M: f32 = 0.05;

/// How far under the land a roof is held where a street crossing over a tunnel
/// leaves no room for the headroom and a full slab, metres.
pub const ROOF_SLAB_M: f32 = 0.3;

/// The least headroom a street crossing over a tunnel roofs it with, metres —
/// only enough that the ceiling cannot stand below the road. **A street is
/// never cut in two to make room**: where the network puts a path too near the
/// road it passes under — a junction at grade seven metres before a
/// carriageway leaves no grade that gets under it, and north of the ring at
/// the Paterswoldseweg one pulls a path up early — the underpass is drawn low,
/// and [`audit`] reports it, rather than opening a slot across the carriageway
/// that a car would then drive into (2026-09-29).
const MIN_HEADROOM_M: f32 = 0.3;

/// The headroom under which [`audit`] reports a roofed stretch: roofed so the
/// street over it stays whole, but lower than any underpass is, so something —
/// the network's grades, the land — is misread there.
const AUDIT_HEADROOM_M: f32 = 1.0;

/// Which of a tunnel's points a street *at grade* passes over: where that
/// street's surface — its line and half-width, away from its own ends and
/// outside every structure — would fall inside the opening an open stretch here
/// cuts (`half` and the half-metre every opening is widened by), and one point
/// either side, since an opening reaches one point into a roof.
///
/// **The flaw's own definition, not a line crossing.** Crossing lines missed a
/// carriageway whose bend only grazed a cycle tunnel's line while its seven
/// metres of surface lay across it. A street beside a cutting is outside the
/// opening; one in the trench is part of the structure; one on a bridge is not
/// at grade — so none of those roofs anything.
fn crossed_by_streets(
    runs: &[StreetRun],
    profiles: &[Option<Profile>],
    ri: usize,
    pts: &[[f32; 2]],
    half: f32,
) -> Vec<bool> {
    let mut out = vec![false; pts.len()];
    if pts.len() < 2 {
        return out;
    }
    let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for p in pts {
        bb = [
            bb[0].min(p[0]),
            bb[1].min(p[1]),
            bb[2].max(p[0]),
            bb[3].max(p[1]),
        ];
    }
    for (oi, o) in runs.iter().enumerate() {
        if oi == ri || o.level != 0 || o.points.len() < 2 {
            continue;
        }
        let reach = half + 0.5 + o.half_w;
        let land = profiles.get(oi).and_then(Option::as_ref).map(|p| &p.land);
        let (first, last) = (o.points[0], o.points[o.points.len() - 1]);
        for (m, &q) in o.points.iter().enumerate() {
            if q[0] < bb[0] - reach
                || q[0] > bb[2] + reach
                || q[1] < bb[1] - reach
                || q[1] > bb[3] + reach
            {
                continue;
            }
            // Near its own ends a street leads out of a ramp or tees into a
            // cutting's side; and one whose end joins this structure is its road
            // out, for as far as its surface could reach the opening.
            let joins = |e: [f32; 2]| {
                pts.iter()
                    .any(|p| (p[0] - e[0]).hypot(p[1] - e[1]) <= road_levels::TEE_M + reach)
            };
            let near_end = |e: [f32; 2]| {
                let d = (e[0] - q[0]).hypot(e[1] - q[1]);
                d < 3.0 || joins(e) && d < reach + 3.0 + road_levels::TEE_M
            };
            if near_end(first)
                || near_end(last)
                || land.and_then(|l| l.get(m)).is_some_and(|g| g.is_finite())
            {
                continue;
            }
            // The street's heading here, to tell one crossing over from one
            // merging alongside — the slip road beside a ramp of the ring.
            let (a, b) = (
                o.points[m.saturating_sub(1)],
                o.points[(m + 1).min(o.points.len() - 1)],
            );
            let along = [b[0] - a[0], b[1] - a[1]];
            let along_len = along[0].hypot(along[1]);
            if along_len <= 1e-6 {
                continue;
            }
            for k in 0..pts.len() - 1 {
                let seg = [pts[k + 1][0] - pts[k][0], pts[k + 1][1] - pts[k][1]];
                let seg_len = seg[0].hypot(seg[1]);
                if seg_len <= 1e-6
                    || ((along[0] * seg[0] + along[1] * seg[1]) / (along_len * seg_len)).abs()
                        >= JOIN_COS
                {
                    continue;
                }
                let d = project(pts[k], pts[k + 1], q).map_or_else(
                    || {
                        (q[0] - pts[k][0])
                            .hypot(q[1] - pts[k][1])
                            .min((q[0] - pts[k + 1][0]).hypot(q[1] - pts[k + 1][1]))
                    },
                    |(d, _)| d,
                );
                if d <= reach {
                    for j in k.saturating_sub(1)..=(k + 2).min(pts.len() - 1) {
                        out[j] = true;
                    }
                }
            }
        }
    }
    out
}

/// Shortest open stretch between two roofs that is opened to the sky, metres.
/// Shorter, it is a seam in the tagging and the roof carries on over it.
const MIN_OPEN_M: f32 = 25.0;

/// The profiles a tile's runs take with nothing measured: the land flat at 0,
/// every street on it, and every tunnel [`level_depth`] under it — with the
/// ramps down to them graded on the tile's own network by the same solve atlas
/// runs over the survey (`road_levels::solve`), and the structures found by the
/// same rule (`road_levels::structures`).
///
/// **This is the whole of the unmeasured case**, and it is deliberately the
/// measured one with the measurements taken out rather than a second planner:
/// where atlas has not been, the tiles say which ways are tunnels and nothing
/// about the ground, so the ground is flat and the road is where the network puts
/// it. What it cannot do is look past its own tile, so a ramp that would cross the
/// border is cut short there — atlas solves over a window and has no such edge.
#[must_use]
pub fn unmeasured_profiles(runs: &[StreetRun]) -> Vec<Option<Profile>> {
    if !runs.iter().any(|r| r.level < 0) {
        return vec![None; runs.len()];
    }
    let lines: Vec<road_levels::Line> = runs
        .iter()
        .map(|r| road_levels::Line {
            points: r.points.clone(),
            heights: r
                .points
                .iter()
                .map(|_| {
                    if r.level < 0 {
                        Height::AtMost {
                            ceiling: -level_depth(r.class),
                            measured: None,
                        }
                    } else {
                        Height::Measured {
                            y: 0.0,
                            weight: 1.0,
                        }
                    }
                })
                .collect(),
            level: r.level,
            max_grade: road_levels::max_grade(r.class),
            cut: r.cut,
        })
        .collect();
    let solved = road_levels::solve(&lines);
    let flat: Vec<Vec<f32>> = runs.iter().map(|r| vec![0.0; r.points.len()]).collect();
    let mask = road_levels::structures(&lines, &solved.heights, &flat);
    solved
        .heights
        .into_iter()
        .zip(mask)
        .map(|(road, inside)| {
            inside.iter().any(|&b| b).then(|| Profile {
                land: inside
                    .iter()
                    .map(|&b| if b { 0.0 } else { f32::NAN })
                    .collect(),
                road,
                walls: Vec::new(),
            })
        })
        .collect()
}

/// Every structure in a tile from the tiles alone ([`unmeasured_profiles`]), on
/// flat land at 0.
#[must_use]
pub fn build_tunnels(tile: &TileData, tile_m: f32, max_bytes: Option<usize>) -> TunnelTessellation {
    let runs = road_levels::street_runs(tile, tile_m);
    build_structures(&runs, &unmeasured_profiles(&runs), max_bytes)
}

/// One place a roof stops with sky beyond it — a tunnel mouth.
#[derive(Clone, Copy, Debug)]
pub struct Mouth {
    /// Where the roof stops, on the centre line, tile-local metres.
    pub at: [f32; 2],
    /// Unit direction out of the tunnel, into the open.
    pub out: [f32; 2],
    /// The carriageway's height there, and the land's, in the profiles' frame.
    pub road_y: f32,
    pub land_y: f32,
    /// The `streets` feature the tunnel is.
    pub feature: usize,
}

/// Every tunnel mouth [`build_structures`] would draw a headwall over, from the
/// same plans it draws them from — so a list of them, and a camera aimed at each,
/// cannot disagree with the geometry about where they are.
#[must_use]
pub fn mouths(runs: &[StreetRun], profiles: &[Option<Profile>]) -> Vec<Mouth> {
    let mut out = Vec::new();
    for p in plan(runs, profiles) {
        let n = p.centre.len();
        for k in 0..n {
            if !p.roofed[k] {
                continue;
            }
            let back_open = if k == 0 { p.mouth[0] } else { !p.roofed[k - 1] };
            let ahead_open = if k + 1 == n {
                p.mouth[1]
            } else {
                !p.roofed[k + 1]
            };
            let mut push = |from: [f32; 2], to: [f32; 2]| {
                let (dx, dz) = (to[0] - from[0], to[1] - from[1]);
                let len = dx.hypot(dz);
                if len > 1e-6 {
                    out.push(Mouth {
                        at: p.centre[k],
                        out: [dx / len, dz / len],
                        road_y: p.road_y[k],
                        land_y: p.land_y[k],
                        feature: p.feature,
                    });
                }
            };
            // Out of the tunnel is away from its roofed neighbour.
            if back_open && k + 1 < n && p.roofed[k + 1] {
                push(p.centre[k + 1], p.centre[k]);
            }
            if ahead_open && k > 0 && p.roofed[k - 1] {
                push(p.centre[k - 1], p.centre[k]);
            }
        }
    }
    out
}

/// A shape a set of structures should never take, as [`audit`] finds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flaw {
    /// An opening in the ground swallows a street at grade that is not part of
    /// any structure: the slot a shallow underpass cut across the road over it.
    SlotThroughStreet,
    /// A roofed stretch whose ceiling stands under [`AUDIT_HEADROOM_M`] over its
    /// road, or above its land.
    CeilingOutOfPlace,
    /// An open end below its land that no tile border, dead-end wall or other
    /// structure's end meets: the carriageway falling off a cliff.
    StepIntoNothing,
}

impl Flaw {
    /// The name the published list and the gallery use.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::SlotThroughStreet => "slot-through-street",
            Self::CeilingOutOfPlace => "ceiling-out-of-place",
            Self::StepIntoNothing => "step-into-nothing",
        }
    }
}

/// One [`Flaw`] where [`audit`] found it, tile-local metres.
#[derive(Clone, Copy, Debug)]
pub struct Finding {
    pub flaw: Flaw,
    pub at: [f32; 2],
}

/// **Every shape the structures in a tile should never take**, from the same
/// plans [`build_structures`] draws.
///
/// Each of these was found once by eye, in a render beside its aerial
/// photograph, and fixed by a rule. A rule can be broken by the next one without
/// any render being looked at, so atlas runs this over every tile it grades and
/// says how many it found, and publishes where, so a change that brings one back
/// is a number that moved rather than a picture nobody took.
#[must_use]
pub fn audit(runs: &[StreetRun], profiles: &[Option<Profile>]) -> Vec<Finding> {
    let plans = plan(runs, profiles);
    let mut out = Vec::new();

    // Ceilings.
    for p in &plans {
        for k in 0..p.centre.len() {
            if p.roofed[k]
                && (p.ceiling[k] < p.road_y[k] + AUDIT_HEADROOM_M - 0.01
                    || p.ceiling[k] > p.land_y[k] + 0.01)
            {
                out.push(Finding {
                    flaw: Flaw::CeilingOutOfPlace,
                    at: p.centre[k],
                });
            }
        }
    }

    // Open ends below the land with nothing beyond.
    let ends: Vec<([f32; 2], f32, f32, bool)> = plans
        .iter()
        .flat_map(|p| {
            let n = p.centre.len() - 1;
            // An end where the street carries on is met by the street itself,
            // laid in the ground from there — the survey's own slope, where a
            // ramp's banks are banks rather than walls.
            [(0usize, 0usize), (1, n)].map(|(k, v)| {
                (
                    p.centre[v],
                    p.road_y[v],
                    p.land_y[v],
                    p.roofed[v] || p.cut[k] || p.dead[k] || !p.run_end[k],
                )
            })
        })
        .collect();
    for (i, &(at, y, g, closed)) in ends.iter().enumerate() {
        if closed || y > g - road_levels::SUNK_M - 0.01 {
            continue;
        }
        let met = ends.iter().enumerate().any(|(j, &(q, oy, ..))| {
            j != i
                && (q[0] - at[0]).hypot(q[1] - at[1]) < road_levels::TEE_M
                && (oy - y).abs() < 0.05
        });
        if !met {
            out.push(Finding {
                flaw: Flaw::StepIntoNothing,
                at,
            });
        }
    }

    // Streets at grade inside an opening.
    let openings = emit(&plans, None, &at_grade(runs, profiles)).openings;
    let bounds: Vec<Bbox> = openings.iter().map(|r| bbox_of_ring(r)).collect();
    for (ri, r) in runs.iter().enumerate() {
        if r.level != 0 {
            continue;
        }
        let land = profiles.get(ri).and_then(Option::as_ref).map(|p| &p.land);
        let (first, last) = (r.points[0], r.points[r.points.len() - 1]);
        for (k, &q) in r.points.iter().enumerate() {
            // Its own ends are where a street leads out of a ramp, or tees into
            // a cutting's side; only a street passing through counts.
            let near_end = |e: [f32; 2]| (e[0] - q[0]).hypot(e[1] - q[1]) < 3.0;
            if near_end(first)
                || near_end(last)
                || land.and_then(|l| l.get(k)).is_some_and(|g| g.is_finite())
            {
                continue;
            }
            let hit = openings
                .iter()
                .zip(&bounds)
                .any(|(ring, b)| point_in_bbox(q, b) && point_in_ring(q, ring));
            if hit {
                out.push(Finding {
                    flaw: Flaw::SlotThroughStreet,
                    at: q,
                });
                break;
            }
        }
    }
    out
}

/// Build every structure below the ground in one tile: `profiles` is parallel to
/// `runs`, `None` for a run that is not part of one.
///
/// `max_bytes` cuts the result into several meshes the way the other builders do,
/// because bevy's upload budget writes one whole asset per frame and cannot spread
/// a single huge one (`docs/notes/upload-bursts.md`).
#[must_use]
pub fn build_structures(
    runs: &[StreetRun],
    profiles: &[Option<Profile>],
    max_bytes: Option<usize>,
) -> TunnelTessellation {
    emit(&plan(runs, profiles), max_bytes, &at_grade(runs, profiles))
}

/// The stretches of each run that are structures, simplified, roofed where a
/// roof fits, and with their mouths found.
fn plan(runs: &[StreetRun], profiles: &[Option<Profile>]) -> Vec<Plan> {
    let mut plans: Vec<Plan> = Vec::new();
    // Every street's points, bucketed, for telling a road that ends in nothing
    // from one that meets another.
    let cell = |p: [f32; 2]| {
        #[allow(clippy::cast_possible_truncation)]
        (
            (p[0] / road_levels::TEE_M).floor() as i64,
            (p[1] / road_levels::TEE_M).floor() as i64,
        )
    };
    let mut near: std::collections::HashMap<(i64, i64), Vec<(usize, [f32; 2])>> =
        std::collections::HashMap::new();
    for (ri, run) in runs.iter().enumerate() {
        for &p in &run.points {
            near.entry(cell(p)).or_default().push((ri, p));
        }
    }
    let alone = |ri: usize, p: [f32; 2]| {
        let (cx, cz) = cell(p);
        !(-1..=1).any(|dx| {
            (-1..=1).any(|dz| {
                near.get(&(cx + dx, cz + dz))
                    .into_iter()
                    .flatten()
                    .any(|&(o, q)| {
                        o != ri && (q[0] - p[0]).hypot(q[1] - p[1]) <= road_levels::TEE_M
                    })
            })
        })
    };
    for (ri, (run, profile)) in runs.iter().zip(profiles).enumerate() {
        let Some(profile) = profile else {
            continue;
        };
        let n = run.points.len();
        if profile.road.len() != n || profile.land.len() != n {
            continue;
        }
        let part = |i: usize| profile.road[i].is_finite() && profile.land[i].is_finite();
        let clearance = clearance(run.class);
        let mut i = 0;
        while i < n {
            if !part(i) {
                i += 1;
                continue;
            }
            let start = i;
            while i < n && part(i) {
                i += 1;
            }
            if i - start < 2 {
                continue;
            }
            let measured: Vec<[f32; 2]> = if profile.walls.len() == n {
                profile.walls[start..i].to_vec()
            } else {
                vec![[f32::NAN; 2]; i - start]
            };
            // Only where it changes the roof — a tunnel deep enough for one
            // already has it, and a flag that breaks the simplification there
            // would move every vertex of it for nothing.
            let crossed: Vec<bool> =
                crossed_by_streets(runs, profiles, ri, &run.points[start..i], run.half_w)
                    .into_iter()
                    .zip(&profile.road[start..i])
                    .zip(&profile.land[start..i])
                    .map(|((c, &y), &g)| c && y + clearance > g + CEILING_MAX_Y)
                    .collect();
            // **A roof goes on where the tiles say the road is under the ground
            // and one fits under the land** — its headroom plus a slab. The rest
            // of a structure is open: the ramps, the cuttings, and the stretch of
            // a short underpass that never gets deep enough to take one.
            //
            // **And never where atlas measured walls**: it measures them only for
            // a stretch open to the sky — a road at grade, or a tunnel's under a
            // lifted deck, which is a road under a span (atlas's `spanned`).
            //
            // **And always where another street crosses over it** ([`crossed_by_streets`]):
            // a cycle underpass two and a half metres under the road it passes
            // below has no room for the headroom and a slab, and left open it
            // cut a slot through the road above the width of the path
            // (2026-09-29). There the ceiling is held under the land instead
            // (`Plan::ceiling`), and the headroom gives.
            //
            // **Decided on every point, before simplifying**, and simplification
            // keeps each point where the answer changes: decided on the kept
            // vertices alone, a tunnel simplified to its two ends was roofed at
            // the deep one and open at the other, and the whole stretch between
            // them — a carriageway's width of it under the road — opened.
            let roof_here: Vec<bool> = profile.road[start..i]
                .iter()
                .zip(&profile.land[start..i])
                .zip(&measured)
                .zip(&crossed)
                .map(|(((&y, &g), w), &over)| {
                    // A street at grade over a cutting can only stand on a lid,
                    // whatever the cutting's level and whatever walls atlas
                    // measured beside it; a bridge over one is not at grade.
                    (over && g - ROOF_SLAB_M - y >= MIN_HEADROOM_M)
                        || run.level < 0
                            && !w.iter().any(|v| v.is_finite())
                            && y + clearance <= g + CEILING_MAX_Y
                })
                .collect();
            let breaks: Vec<u8> = roof_here
                .iter()
                .zip(&crossed)
                .map(|(&r, &c)| u8::from(r) | (u8::from(c) << 1))
                .collect();
            let (centre, road_y, land_y, walls, breaks) = simplified(
                &run.points[start..i],
                &profile.road[start..i],
                &profile.land[start..i],
                &measured,
                &breaks,
            );
            let roofed: Vec<bool> = breaks.iter().map(|b| b & 1 == 1).collect();
            let crossed: Vec<bool> = breaks.iter().map(|b| b & 2 == 2).collect();
            let ceiling: Vec<f32> = road_y
                .iter()
                .zip(&land_y)
                .map(|(&y, &g)| ceiling_over(y, g, clearance))
                .collect();
            let mut travelled = Vec::with_capacity(centre.len());
            let mut total = 0.0f32;
            for k in 0..centre.len() {
                if k > 0 {
                    total +=
                        (centre[k][0] - centre[k - 1][0]).hypot(centre[k][1] - centre[k - 1][1]);
                }
                travelled.push(total);
            }
            plans.push(Plan {
                centre,
                travelled,
                road_y,
                land_y,
                walls,
                crossed,
                ceiling,
                roofed,
                class: run.class,
                half: run.half_w,
                clearance,
                feature: run.feature,
                cut: [start == 0 && run.cut[0], i == n && run.cut[1]],
                mouth: [false; 2],
                run_end: [start == 0, i == n],
                dead: [
                    start == 0 && !run.cut[0] && alone(ri, run.points[0]),
                    i == n && !run.cut[1] && alone(ri, run.points[n - 1]),
                ],
            });
        }
    }

    // Where each plan's ends are, and what meets there.
    let key = |p: [f32; 2]| {
        #[allow(clippy::cast_possible_truncation)]
        ((p[0] * 100.0).round() as i64, (p[1] * 100.0).round() as i64)
    };

    // **A short gap between two roofs is a seam in the tagging, not a stretch of
    // sky.** The road runs through either way — that is what the structure is
    // for — but taking the ground away over it is a claim the tiles never made:
    // on the fixture two path tunnels end four metres apart, and opening that
    // punched a 5 × 4 m hole in the ground over what is plainly one tunnel.
    let roofed_ends: std::collections::HashMap<(i64, i64), usize> =
        plans
            .iter()
            .fold(std::collections::HashMap::new(), |mut m, p| {
                let last = p.centre.len() - 1;
                for v in [0, last] {
                    if p.roofed[v] {
                        *m.entry(key(p.centre[v])).or_insert(0) += 1;
                    }
                }
                m
            });
    for p in &mut plans {
        let last = p.centre.len() - 1;
        let between_roofs = p.roofed.iter().all(|r| !r)
            && [0, last]
                .iter()
                .all(|&v| roofed_ends.get(&key(p.centre[v])).copied().unwrap_or(0) > 0);
        if between_roofs && p.travelled[last] < MIN_OPEN_M {
            p.roofed = p
                .road_y
                .iter()
                .zip(&p.land_y)
                .zip(&p.walls)
                .zip(&p.crossed)
                .map(|(((&y, &g), w), &over)| {
                    (over && g - ROOF_SLAB_M - y >= MIN_HEADROOM_M)
                        || !w.iter().any(|v| v.is_finite()) && y + p.clearance <= g + CEILING_MAX_Y
                })
                .collect();
        }
    }
    let mut ends: std::collections::HashMap<(i64, i64), Vec<(usize, usize)>> =
        std::collections::HashMap::new();
    for (pi, p) in plans.iter().enumerate() {
        let last = p.centre.len() - 1;
        ends.entry(key(p.centre[0])).or_default().push((pi, 0));
        ends.entry(key(p.centre[last]))
            .or_default()
            .push((pi, last));
    }
    let halves: Vec<f32> = plans.iter().map(|p| p.half).collect();
    for (pi, p) in plans.iter_mut().enumerate() {
        let last = p.centre.len() - 1;
        for (k, v) in [(0usize, 0usize), (1, last)] {
            let others: Vec<(usize, usize)> = ends[&key(p.centre[v])]
                .iter()
                .copied()
                .filter(|&(o, _)| o != pi)
                .collect();
            // **A mouth is a roof that stops with sky beyond it.** Not at a tile
            // border, where the same roof carries on in the next tile, and not
            // where another roofed stretch carries on from the same point (below)
            // — a tunnel mapped as two ways end to end is one tunnel.
            p.mouth[k] = p.roofed[v] && !p.cut[k] && !p.dead[k];
            // **An open stretch takes the width of the roofed one it opens out of**,
            // so the hole it cuts swallows the lid's walls rather than stepping in
            // at the portal where two carriageway widths disagree.
            if !p.roofed[v] {
                for &(o, _) in &others {
                    p.half = p.half.max(halves[o]);
                }
            }
        }
    }
    // The roofed ends that meet another roofed end are not mouths.
    let roofed_at: std::collections::HashMap<(i64, i64), usize> = plans
        .iter()
        .flat_map(|p| {
            let last = p.centre.len() - 1;
            [(0usize, 0usize), (1, last)]
                .into_iter()
                .filter(|&(_, v)| p.roofed[v])
                .map(|(_, v)| key(p.centre[v]))
                .collect::<Vec<_>>()
        })
        .fold(std::collections::HashMap::new(), |mut m, k| {
            *m.entry(k).or_insert(0) += 1;
            m
        });
    for p in &mut plans {
        let last = p.centre.len() - 1;
        for (k, v) in [(0usize, 0usize), (1, last)] {
            if p.mouth[k] && roofed_at.get(&key(p.centre[v])).copied().unwrap_or(0) > 1 {
                p.mouth[k] = false;
            }
        }
    }
    plans
}

/// A stretch with the points dropped that change nothing: each kept point is the
/// last from which the next can be reached in a straight line — in plan and in
/// both heights — within [`SIMPLIFY_M`]. The ends are always kept.
fn simplified(
    points: &[[f32; 2]],
    road: &[f32],
    land: &[f32],
    walls: &[[f32; 2]],
    breaks: &[u8],
) -> (Vec<[f32; 2]>, Vec<f32>, Vec<f32>, Vec<[f32; 2]>, Vec<u8>) {
    // A wall's kind — none, measured, at the carriageway — never changes between
    // two kept points, and a measured one only within the tolerance.
    let kind = |w: f32| {
        if w.is_nan() {
            0u8
        } else if w > 0.0 {
            1
        } else {
            2
        }
    };
    let n = points.len();
    let mut keep = vec![0usize];
    let mut anchor = 0usize;
    let mut i = 2usize;
    while i < n {
        // Can every point strictly between `anchor` and `i` be dropped?
        let (a, b) = (points[anchor], points[i]);
        let len = (b[0] - a[0]).hypot(b[1] - a[1]).max(1e-6);
        let fits = (anchor + 1..i).all(|j| {
            let p = points[j];
            let t = (((p[0] - a[0]) * (b[0] - a[0]) + (p[1] - a[1]) * (b[1] - a[1])) / (len * len))
                .clamp(0.0, 1.0);
            let off = (p[0] - (a[0] + (b[0] - a[0]) * t)).hypot(p[1] - (a[1] + (b[1] - a[1]) * t));
            let on = |v: &[f32]| (v[j] - (v[anchor] + (v[i] - v[anchor]) * t)).abs();
            let wall = |s: usize| {
                let (a, m, b) = (walls[anchor][s], walls[j][s], walls[i][s]);
                kind(a) == kind(m)
                    && kind(m) == kind(b)
                    && (kind(m) != 1 || (m - (a + (b - a) * t)).abs() <= SIMPLIFY_M)
            };
            off <= SIMPLIFY_M
                && on(road) <= SIMPLIFY_M
                && on(land) <= SIMPLIFY_M
                && wall(0)
                && wall(1)
                && breaks[anchor] == breaks[j]
                && breaks[j] == breaks[i]
        });
        if !fits {
            keep.push(i - 1);
            anchor = i - 1;
        }
        i += 1;
    }
    keep.push(n - 1);
    keep.dedup();
    (
        keep.iter().map(|&k| points[k]).collect(),
        keep.iter().map(|&k| road[k]).collect(),
        keep.iter().map(|&k| land[k]).collect(),
        keep.iter().map(|&k| walls[k]).collect(),
        keep.iter().map(|&k| breaks[k]).collect(),
    )
}

/// Widest gap between two carriageways' edges that is a median rather than
/// ground between two roads, metres. A Dutch dual carriageway in a cutting
/// keeps a barrier and a verge between its halves; twelve metres takes a wide
/// one and stops short of a road beside it.
const MEDIAN_GAP_M: f32 = 12.0;

/// Height of the barrier down the middle of a median, metres.
const BARRIER_M: f32 = 0.8;

/// How far under the carriageways a median's floor is laid, metres, so the
/// decks either side win the depth test where they overlap it.
const MEDIAN_FLOOR_M: f32 = 0.03;

/// The other half of the road a segment's side faces across a median.
#[derive(Clone, Copy, Debug)]
struct Partner {
    plan: usize,
}

/// For every open segment of every plan, which side (left, right) faces the
/// other half of its road across a median.
///
/// **One trench, not two.** A dual carriageway is two ways in OSM and so two
/// structures here, each walled on both sides — so an open cutting drew two
/// walls the full depth of the cutting standing face to face down its middle,
/// with a slot of void between them where the photograph of the DUO cutting
/// shows one floor and a barrier (2026-09-28). The opening over the median was
/// already one hole (`merge_parallel_openings`); this is what lets the walls and
/// the floor agree with it. Same class, parallel, open on both sides, and edges
/// no more than [`MEDIAN_GAP_M`] apart.
fn medians(plans: &[Plan]) -> Vec<Vec<[Option<Partner>; 2]>> {
    let mut out: Vec<Vec<[Option<Partner>; 2]>> = plans
        .iter()
        .map(|p| vec![[None; 2]; p.centre.len().saturating_sub(1)])
        .collect();
    for (pi, a) in plans.iter().enumerate() {
        for i in 0..a.centre.len().saturating_sub(1) {
            if a.roofed[i] && a.roofed[i + 1] {
                continue;
            }
            let (p0, p1) = (a.centre[i], a.centre[i + 1]);
            let (dx, dz) = (p1[0] - p0[0], p1[1] - p0[1]);
            let len = dx.hypot(dz);
            if len <= 1e-3 {
                continue;
            }
            let dir = [dx / len, dz / len];
            let left = [-dir[1], dir[0]];
            let mid = [(p0[0] + p1[0]) * 0.5, (p0[1] + p1[1]) * 0.5];
            let mut best: Option<(f32, usize, bool)> = None;
            for (pj, b) in plans.iter().enumerate() {
                if pj == pi || b.class != a.class {
                    continue;
                }
                for k in 0..b.centre.len().saturating_sub(1) {
                    if b.roofed[k] && b.roofed[k + 1] {
                        continue;
                    }
                    let (q0, q1) = (b.centre[k], b.centre[k + 1]);
                    let (ex, ez) = (q1[0] - q0[0], q1[1] - q0[1]);
                    let blen = ex.hypot(ez);
                    if blen <= 1e-3 || ((dir[0] * ex + dir[1] * ez) / blen).abs() < JOIN_COS {
                        continue;
                    }
                    let Some((d, t)) = project(q0, q1, mid) else {
                        continue;
                    };
                    let gap = d - a.half - b.half;
                    if !(0.0..=MEDIAN_GAP_M).contains(&gap) {
                        continue;
                    }
                    let q = [q0[0] + ex * t, q0[1] + ez * t];
                    let on_left = (q[0] - mid[0]) * left[0] + (q[1] - mid[1]) * left[1] > 0.0;
                    if best.is_none_or(|(bd, ..)| d < bd) {
                        best = Some((d, pj, on_left));
                    }
                }
            }
            if let Some((_, pj, on_left)) = best {
                out[pi][i][usize::from(!on_left)] = Some(Partner { plan: pj });
            }
        }
    }
    out
}

/// The nearest point on a plan's centre line to `p`, and the road height there.
fn nearest_on(plan: &Plan, p: [f32; 2]) -> Option<([f32; 2], f32)> {
    let mut best: Option<(f32, [f32; 2], f32)> = None;
    for k in 0..plan.centre.len().saturating_sub(1) {
        let (a, b) = (plan.centre[k], plan.centre[k + 1]);
        let Some((d, t)) = project(a, b, p) else {
            continue;
        };
        if best.is_none_or(|(bd, ..)| d < bd) {
            let q = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            let y = plan.road_y[k] + (plan.road_y[k + 1] - plan.road_y[k]) * t;
            best = Some((d, q, y));
        }
    }
    best.map(|(_, q, y)| (q, y))
}

/// The floor across a median, and the barrier down its middle, for segment `i`
/// of `a` on its `side` (0 left, 1 right), facing `b`.
fn median_floor(shell: &mut Build, a: &Plan, i: usize, side: usize, b: &Plan) {
    let sides = offset_sides(&a.centre, a.half);
    let edge = if side == 0 {
        sides.left[i]
    } else {
        sides.right[i]
    };
    let (ya0, ya1) = (a.road_y[i], a.road_y[i + 1]);
    let (Some((q0, yb0)), Some((q1, yb1))) = (nearest_on(b, edge[0]), nearest_on(b, edge[1]))
    else {
        return;
    };
    // The partner's edge facing back across the median.
    let toward = |q: [f32; 2], e: [f32; 2]| {
        let (dx, dz) = (e[0] - q[0], e[1] - q[1]);
        let len = dx.hypot(dz).max(1e-6);
        [q[0] + dx / len * b.half, q[1] + dz / len * b.half]
    };
    let (f0, f1) = (toward(q0, edge[0]), toward(q1, edge[1]));
    // Where the two halves touch there is no median to floor, and a strip of no
    // width is a sliver wound whichever way rounding puts it.
    let width = |e: [f32; 2], f: [f32; 2]| (e[0] - f[0]).hypot(e[1] - f[1]);
    if width(edge[0], f0).min(width(edge[1], f1)) < 0.1 {
        return;
    }
    let floor = [
        [edge[0][0], ya0 - MEDIAN_FLOOR_M, edge[0][1]],
        [edge[1][0], ya1 - MEDIAN_FLOOR_M, edge[1][1]],
        [f1[0], yb1 - MEDIAN_FLOOR_M, f1[1]],
        [f0[0], yb0 - MEDIAN_FLOOR_M, f0[1]],
    ];
    let quad = if faces_up(edge[0], edge[1], f1) {
        floor
    } else {
        [floor[1], floor[0], floor[3], floor[2]]
    };
    shell.quad(
        quad,
        [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        UP,
        0.0,
    );
    // The barrier: both faces, down the middle of the median.
    let mid = |e: [f32; 2], f: [f32; 2], ya: f32, yb: f32| {
        ([(e[0] + f[0]) * 0.5, (e[1] + f[1]) * 0.5], (ya + yb) * 0.5)
    };
    let ((m0, h0), (m1, h1)) = (mid(edge[0], f0, ya0, yb0), mid(edge[1], f1, ya1, yb1));
    for left in [true, false] {
        let n = wall_normal(m0, m1, left);
        let (lo0, lo1, hi0, hi1) = (
            [m0[0], h0, m0[1]],
            [m1[0], h1, m1[1]],
            [m0[0], h0 + BARRIER_M, m0[1]],
            [m1[0], h1 + BARRIER_M, m1[1]],
        );
        let face = if left {
            [lo1, lo0, hi0, hi1]
        } else {
            [lo0, lo1, hi1, hi0]
        };
        shell.quad(
            face,
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            n,
            0.0,
        );
    }
}

/// How far from the centre line side `s` of a plan's wall stands at vertex `k`,
/// or `None` where that side is a bank and has no wall. Under a roof the walls
/// are the carriageway's; where nothing was measured, likewise.
fn reach(plan: &Plan, k: usize, s: usize, covered: bool) -> Option<f32> {
    if covered {
        return Some(plan.half);
    }
    let w = plan.walls.get(k).map_or(f32::NAN, |w| w[s]);
    if w.is_nan() {
        Some(plan.half)
    } else if w > 0.0 {
        Some(w.max(plan.half))
    } else {
        None
    }
}

/// Each segment's two walls as plan lines, left then right: `None` on a side
/// that is a bank (`reach`) or faces the other half of the road across a median
/// ([`medians`]).
fn wall_lines(plan: &Plan, facing: &[[Option<Partner>; 2]]) -> Vec<[Option<[[f32; 2]; 2]>; 2]> {
    let c = &plan.centre;
    (0..c.len().saturating_sub(1))
        .map(|i| {
            let (a, b) = (c[i], c[i + 1]);
            let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
            let len = dx.hypot(dz).max(1e-6);
            let n = [-dz / len, dx / len];
            let covered = plan.roofed[i] && plan.roofed[i + 1];
            let line = |s: usize| {
                if facing[i][s].is_some() {
                    return None;
                }
                let sg = if s == 0 { 1.0 } else { -1.0 };
                let (o0, o1) = (reach(plan, i, s, covered)?, reach(plan, i + 1, s, covered)?);
                Some([
                    [a[0] + n[0] * sg * o0, a[1] + n[1] * sg * o0],
                    [b[0] + n[0] * sg * o1, b[1] + n[1] * sg * o1],
                ])
            };
            [line(0), line(1)]
        })
        .collect()
}

/// How far out each vertex's opening in the ground must reach, left then right:
/// to its wall where the wall stands back from the carriageway, to the
/// carriageway where it does not or where the side is a bank.
fn reaches(plan: &Plan) -> Vec<[f32; 2]> {
    (0..plan.centre.len())
        .map(|k| {
            let covered = plan.roofed[k];
            [0, 1].map(|s| reach(plan, k, s, covered).unwrap_or(plan.half))
        })
        .collect()
}

/// Floor from a carriageway's edge out to a wall standing back from it, just
/// under the road so the deck wins where the two meet.
fn floor_strip(shell: &mut Build, edge: [[f32; 2]; 2], wall: [[f32; 2]; 2], y: [f32; 2]) {
    let wide = |e: [f32; 2], w: [f32; 2]| (e[0] - w[0]).hypot(e[1] - w[1]) > 0.05;
    if !wide(edge[0], wall[0]) && !wide(edge[1], wall[1]) {
        return;
    }
    let (y0, y1) = (y[0] - MEDIAN_FLOOR_M, y[1] - MEDIAN_FLOOR_M);
    let q = [
        [edge[0][0], y0, edge[0][1]],
        [edge[1][0], y1, edge[1][1]],
        [wall[1][0], y1, wall[1][1]],
        [wall[0][0], y0, wall[0][1]],
    ];
    let q = if faces_up(edge[0], edge[1], wall[1]) {
        q
    } else {
        [q[1], q[0], q[3], q[2]]
    };
    shell.quad(q, [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], UP, 0.0);
}

/// Where each plan's end wings start, per end and side: the distance from the
/// centre line out to which whatever the end meets already walls the trench —
/// the widest wall among the plans ending at the same point, on the same side —
/// or `None` for no wing (a tile border, where the structure carries on, and a
/// dead end, which has its own end wall).
fn wing_starts(plans: &[Plan], facing: &[Vec<[Option<Partner>; 2]>]) -> Vec<[[Option<f32>; 2]; 2]> {
    let key = |p: [f32; 2]| {
        #[allow(clippy::cast_possible_truncation)]
        ((p[0] * 100.0).round() as i64, (p[1] * 100.0).round() as i64)
    };
    let lines: Vec<_> = plans
        .iter()
        .zip(facing)
        .map(|(p, f)| wall_lines(p, f))
        .collect();
    // Each plan's end: where it is, which end, and how far out each side's wall
    // stands there (its half where it has none).
    let reach_at = |pi: usize, k: usize| -> [f32; 2] {
        let p = &plans[pi];
        let (seg, v) = if k == 0 {
            (0, 0)
        } else {
            (p.centre.len() - 2, p.centre.len() - 1)
        };
        [0, 1].map(|s| {
            lines[pi][seg][s].map_or(p.half, |w| {
                let q = w[usize::from(k == 1)];
                (q[0] - p.centre[v][0]).hypot(q[1] - p.centre[v][1])
            })
        })
    };
    let mut at: std::collections::HashMap<(i64, i64), Vec<(usize, usize)>> =
        std::collections::HashMap::new();
    for (pi, p) in plans.iter().enumerate() {
        if p.centre.len() < 2 {
            continue;
        }
        at.entry(key(p.centre[0])).or_default().push((pi, 0));
        at.entry(key(p.centre[p.centre.len() - 1]))
            .or_default()
            .push((pi, 1));
    }
    plans
        .iter()
        .enumerate()
        .map(|(pi, p)| {
            if p.centre.len() < 2 {
                return [[None; 2]; 2];
            }
            [0usize, 1].map(|k| {
                if p.cut[k] || p.dead[k] {
                    return [None; 2];
                }
                let v = if k == 0 { 0 } else { p.centre.len() - 1 };
                let mut from = [p.half; 2];
                for &(o, ko) in &at[&key(p.centre[v])] {
                    if o == pi {
                        continue;
                    }
                    let r = reach_at(o, ko);
                    // Two plans running on through the point share their sides;
                    // two meeting head to head, or leaving it back to back, swap
                    // them.
                    let r = if ko == k { [r[1], r[0]] } else { r };
                    from = [from[0].max(r[0]), from[1].max(r[1])];
                }
                from.map(Some)
            })
        })
        .collect()
}

/// Build the meshes, corridors and openings for a set of settled plans.
/// Every point of every street at grade that is not part of a structure, away
/// from its own ends — what no opening may swallow ([`Flaw::SlotThroughStreet`]).
fn at_grade(runs: &[StreetRun], profiles: &[Option<Profile>]) -> Vec<[f32; 2]> {
    let mut out = Vec::new();
    for (ri, r) in runs.iter().enumerate() {
        if r.level != 0 || r.points.len() < 2 {
            continue;
        }
        let land = profiles.get(ri).and_then(Option::as_ref).map(|p| &p.land);
        let (first, last) = (r.points[0], r.points[r.points.len() - 1]);
        for (k, &q) in r.points.iter().enumerate() {
            let near_end = |e: [f32; 2]| (e[0] - q[0]).hypot(e[1] - q[1]) < 3.0;
            if !(near_end(first)
                || near_end(last)
                || land.and_then(|l| l.get(k)).is_some_and(|g| g.is_finite()))
            {
                out.push(q);
            }
        }
    }
    out
}

fn emit(plans: &[Plan], max_bytes: Option<usize>, streets: &[[f32; 2]]) -> TunnelTessellation {
    let mut out = TunnelTessellation::default();
    // **One deck builder per class, not one for the tile.** A tunnel's carriageway
    // is the surface road continued, so it wants that road's own texture: gravel
    // under a footpath, tarmac and lane markings under a trunk road. This was a
    // single builder set by whichever tunnel came first, so a cell with a cycle
    // underpass and a motorway tunnel drew both as whichever it met first —
    // visibly, over Groningen's ring, as a motorway surfaced like a towpath
    // (2026-09-20).
    let mut decks: Vec<(RoadClass, Build)> = Vec::new();
    let mut shell = Build::default();
    let mut meshes: Vec<SurfaceMesh> = Vec::new();
    // Openings carry the corridor they came out of until the merge below has had
    // its say — see [`merge_parallel_openings`].
    let mut openings: Vec<(RoadClass, f32, Vec<[f32; 2]>)> = Vec::new();
    let facing = medians(plans);
    let wings = wing_starts(plans, &facing);

    for (pi, plan) in plans.iter().enumerate() {
        let facing = &facing[pi];
        let Plan {
            centre,
            travelled,
            road_y,
            land_y,
            ceiling,
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
        // **A carriageway below the ground asks the depth buffer for nothing.** A
        // tunnel is held under the ground by metres of real geometry, so there is
        // no coplanar fight here for a bias to settle — and the bridge rung it once
        // asked for drew the tunnel *over* the ground past about 2.3 km
        // (`a_tunnel_asks_for_no_more_depth_bias_than_the_road_band_holds`).
        let rung = road_depth_rung(false);
        // The top of a wall at a vertex: under a roof the ceiling, where it is
        // open the land it holds back.
        let top = |k: usize, covered: bool| {
            if covered {
                ceiling[k]
            } else {
                land_y[k]
            }
        };
        let walls_of = wall_lines(plan, facing);

        for i in 0..sides.left.len() {
            let (l0, l1) = (sides.left[i][0], sides.left[i][1]);
            let (r0, r1) = (sides.right[i][0], sides.right[i][1]);
            let (y0, y1) = (road_y[i], road_y[i + 1]);
            let (v0, v1) = (travelled[i] / tex, travelled[i + 1] / tex);

            // Deck, wound to face up — left edge first in this frame (x east,
            // y south). Wound the other way it is backface-culled from every
            // angle anybody looks at a road from (`a_tunnel_deck_faces_up`).
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

            // Walls, inward-facing in the winding as well as in the normal: the
            // material culls back faces, and the outer face is buried in the
            // ground no camera reaches (`every_tunnel_face_is_wound_toward_its_normal`).
            //
            // **Up to the ceiling under a roof, and to the land only where it is
            // open.** Between a roof and the ground over it is earth, which has
            // no mesh; a wall carried on up to the land stood out of anything lower
            // — along the ring's lid under the Oude Winschoterdiep the two wall
            // tops ran out of the canal as long thin slivers (2026-09-23).
            let covered = roofed[i] && roofed[i + 1];
            let (t0, t1) = (top(i, covered), top(i + 1, covered));
            // Not on a side that faces the other half of the road across a
            // median: that is one trench, and its floor is laid below.
            //
            // **Where the survey put it** (`walls_of`, `Profile::walls`): past the
            // carriageway where the trench is wider than the road, and not at all
            // on a side where the ground comes down as a bank. Between the
            // carriageway and a wall standing back from it is floor.
            if let Some([w0, w1]) = walls_of[i][0] {
                let n_left = wall_normal(w0, w1, true);
                shell.quad(
                    [
                        [w1[0], y1, w1[1]],
                        [w0[0], y0, w0[1]],
                        [w0[0], t0, w0[1]],
                        [w1[0], t1, w1[1]],
                    ],
                    [[v1, 0.0], [v0, 0.0], [v0, 1.0], [v1, 1.0]],
                    n_left,
                    0.0,
                );
                floor_strip(&mut shell, [l0, l1], [w0, w1], [y0, y1]);
            }
            if let Some([w0, w1]) = walls_of[i][1] {
                let n_right = wall_normal(w0, w1, false);
                shell.quad(
                    [
                        [w0[0], y0, w0[1]],
                        [w1[0], y1, w1[1]],
                        [w1[0], t1, w1[1]],
                        [w0[0], t0, w0[1]],
                    ],
                    [[v0, 0.0], [v1, 0.0], [v1, 1.0], [v0, 1.0]],
                    n_right,
                    0.0,
                );
                floor_strip(&mut shell, [r0, r1], [w0, w1], [y0, y1]);
            }

            // Roof, wound to face *down*: wound like the deck it faced the earth
            // above it and was culled from underneath — a lid you could see the
            // sky through.
            let (c0, c1) = (ceiling[i], ceiling[i + 1]);
            if covered {
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
            // side and the roof. **Only on the outside**: on the inside the two
            // offset edges cross, so its triangle is a real one wound backwards and
            // its wall quad stands inside the carriageway facing the wrong way. The
            // side is the one whose triangle comes out facing up — the same test the
            // deck is held to — so the rule cannot disagree with the winding.
            if i + 1 < sides.left.len() {
                let p = centre[i + 1];
                let y = road_y[i + 1];
                let c = ceiling[i + 1];
                let v = v1;
                let next_l = sides.left[i + 1][0];
                let next_r = sides.right[i + 1][0];
                let roof = roofed[i + 1];
                for (side, (a, b, u)) in [(l1, next_l, 0.0), (next_r, r1, 1.0)]
                    .into_iter()
                    .enumerate()
                {
                    if !faces_up(p, a, b) {
                        continue;
                    }
                    let open_median = facing[i][side].is_some() || facing[i + 1][side].is_some();
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
                    // points, facing back toward the centre of the bend. Without
                    // it a bend is a slot you can see the void through.
                    if open_median {
                        continue;
                    }
                    // Between the two walls, where they stand, and the floor
                    // between them and the carriageway's own notch.
                    let (Some(wa), Some(wb)) = (walls_of[i][side], walls_of[i + 1][side]) else {
                        continue;
                    };
                    let (wa, wb) = if side == 0 {
                        (wa[1], wb[0])
                    } else {
                        (wb[0], wa[1])
                    };
                    floor_strip(&mut shell, [a, b], [wa, wb], [y, y]);
                    let (a, b) = (wa, wb);
                    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
                    let len = dx.hypot(dz).max(1e-6);
                    let t = top(i + 1, roof);
                    shell.quad(
                        [
                            [b[0], y, b[1]],
                            [a[0], y, a[1]],
                            [a[0], t, a[1]],
                            [b[0], t, b[1]],
                        ],
                        [[v, 0.0], [v, 0.0], [v, 1.0], [v, 1.0]],
                        [dz / len, 0.0, -dx / len],
                        0.0,
                    );
                }
            }
        }

        // **A headwall over every mouth.** Where a roof stops, the ground over it
        // stops too — it is cut away over the open stretch beyond — and between
        // the roof's edge and the land there was nothing: the earth has no mesh,
        // so from the cutting the mouth showed the lid's culled underside and the
        // grass on top of it (2026-09-23). A vertical face across the corridor
        // from the ceiling up to the land closes it, facing out toward the open
        // side.
        let n = centre.len();
        for k in 0..n {
            if !roofed[k] {
                continue;
            }
            // (left and right corner, facing).
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
            let (c, g) = (ceiling[k], land_y[k]);
            for (l, r, d) in faces {
                let len = d[0].hypot(d[1]).max(1e-6);
                let out_dir = [d[0] / len, 0.0, d[1] / len];
                // Wound to face `out_dir`, whichever way round the corners lie
                // against it — a mouth faces forward at one end and back at the
                // other.
                let forward = (r[0] - l[0]) * d[1] - (r[1] - l[1]) * d[0] > 0.0;
                let (p, q) = if forward { (l, r) } else { (r, l) };
                shell.quad(
                    [
                        [p[0], c, p[1]],
                        [q[0], c, q[1]],
                        [q[0], g, q[1]],
                        [p[0], g, p[1]],
                    ],
                    [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                    out_dir,
                    0.0,
                );
            }
        }

        // **An end wall where the road ends in nothing below its land.** A tunnel
        // into a garage the map does not draw, or a stub of path: open to the sky
        // or under a roof, the end of the corridor showed the void past the
        // carriageway. It is closed from the road up to the wall tops, facing back
        // into the structure.
        let last = n - 1;
        for (k, v, (l, r), inward) in [
            (
                0usize,
                0usize,
                (sides.left[0][0], sides.right[0][0]),
                [centre[1][0] - centre[0][0], centre[1][1] - centre[0][1]],
            ),
            (
                1,
                last,
                (sides.left[last - 1][1], sides.right[last - 1][1]),
                [
                    centre[last - 1][0] - centre[last][0],
                    centre[last - 1][1] - centre[last][1],
                ],
            ),
        ] {
            let seg = if k == 0 { 0 } else { last - 1 };
            let end = usize::from(k == 1);
            let (l, r) = (
                walls_of[seg][0].map_or(l, |w| w[end]),
                walls_of[seg][1].map_or(r, |w| w[end]),
            );
            if !plan.dead[k] || road_y[v] >= land_y[v] - 0.05 {
                continue;
            }
            let t = top(v, roofed[v]);
            let len = inward[0].hypot(inward[1]).max(1e-6);
            let face = [inward[0] / len, 0.0, inward[1] / len];
            let forward = (r[0] - l[0]) * inward[1] - (r[1] - l[1]) * inward[0] > 0.0;
            let (p, q) = if forward { (l, r) } else { (r, l) };
            let y = road_y[v];
            shell.quad(
                [
                    [p[0], y, p[1]],
                    [q[0], y, q[1]],
                    [q[0], t, q[1]],
                    [p[0], t, p[1]],
                ],
                [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                face,
                0.0,
            );
        }

        // **A wing where a trench is wider than what it meets.** The ramp down to
        // a cycle underpass stands its walls back where the survey has them, the
        // bore it runs into is the width of its path, and between the two the
        // opening has taken the ground away from over earth that has no mesh.
        for (k, from) in wings[pi].iter().enumerate() {
            let v = if k == 0 { 0 } else { n - 1 };
            let seg = if k == 0 { 0 } else { n - 2 };
            let inward = if k == 0 {
                [centre[1][0] - centre[0][0], centre[1][1] - centre[0][1]]
            } else {
                [
                    centre[n - 2][0] - centre[n - 1][0],
                    centre[n - 2][1] - centre[n - 1][1],
                ]
            };
            let len = inward[0].hypot(inward[1]).max(1e-6);
            let face = [inward[0] / len, 0.0, inward[1] / len];
            let normal = [-face[2], face[0]];
            let sign = if k == 0 { 1.0 } else { -1.0 };
            for (side, from) in from.iter().enumerate() {
                let Some(from) = *from else { continue };
                let Some(w) = walls_of[seg][side] else {
                    continue;
                };
                let to = w[usize::from(k == 1)];
                let p = centre[v];
                let reach = (to[0] - p[0]).hypot(to[1] - p[1]);
                if reach <= from + 0.05 {
                    continue;
                }
                // The side's unit normal, pointing out toward the wall.
                let s = if side == 0 { sign } else { -sign };
                let at = |d: f32| [p[0] + normal[0] * s * d, p[1] + normal[1] * s * d];
                let (l, r) = (at(from), to);
                let forward = (r[0] - l[0]) * inward[1] - (r[1] - l[1]) * inward[0] > 0.0;
                let (a, b) = if forward { (l, r) } else { (r, l) };
                let (y, t) = (road_y[v], land_y[v]);
                shell.quad(
                    [
                        [a[0], y, a[1]],
                        [b[0], y, b[1]],
                        [b[0], t, b[1]],
                        [a[0], t, a[1]],
                    ],
                    [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                    face,
                    0.0,
                );
            }
        }

        // The roofless stretches, as plan corridors for the caller to cut out of
        // the ground. Keyed on the roof being missing, so the hole and the absent
        // ceiling are decided by one test and cannot disagree about where the
        // opening is.
        openings.extend(
            opening_rings(centre, roofed, half, &reaches(plan))
                .into_iter()
                .map(|ring| (class, half, ring)),
        );

        out.tunnels.push(Tunnel {
            centre: centre.clone(),
            road_y: road_y.clone(),
            land_y: land_y.clone(),
            half_w: half,
            clearance,
            covered: roofed.clone(),
        });
        // The same corridor, keyed to its street, for the drive network — only
        // where the road genuinely runs below its land.
        if road_y.iter().zip(land_y).any(|(y, g)| *y < g - 0.05) {
            out.sunken.push(SunkenWay {
                feature: plan.feature,
                centre: centre.clone(),
                road_y: road_y.clone(),
                half_w: half,
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

    // The floor across every median, laid once from the lower-numbered half.
    for (pi, plan) in plans.iter().enumerate() {
        for (i, sides) in facing[pi].iter().enumerate() {
            for (side, partner) in sides.iter().enumerate() {
                if let Some(p) = partner.filter(|p| p.plan > pi) {
                    median_floor(&mut shell, plan, i, side, &plans[p.plan]);
                }
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
    // One trench, not two slots with a wall of earth between them: a dual
    // carriageway is two ways in OSM and so two structures here, each opening
    // only its own width — which leaves the median standing as ground down the
    // middle of what is one box in the world.
    // What no merged opening may swallow: every roofed stretch, and every
    // street at grade.
    let mut keep_out: Vec<[f32; 2]> = streets.to_vec();
    for p in plans {
        for k in 0..p.centre.len().saturating_sub(1) {
            if p.roofed[k] && p.roofed[k + 1] {
                let (a, b) = (p.centre[k], p.centre[k + 1]);
                for t in [0.1f32, 0.5, 0.9] {
                    keep_out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
                }
            }
        }
    }
    out.openings = merge_parallel_openings(openings, &keep_out);
    out.meshes = meshes;
    out
}

/// Farthest apart two corridors may be and still be one trench, in metres.
///
/// A Dutch dual carriageway's halves are mapped 10–20 m apart. Thirty takes those
/// and stops well short of the next parallel street.
const MEDIAN_M: f32 = 30.0;

/// How nearly two corridors must point the same way to be one trench, as a dot
/// product: `cos 35°`. Sign-free, because the two halves of a dual carriageway run
/// opposite ways.
const JOIN_COS: f32 = 0.819;

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
fn merge_parallel_openings(
    openings: Vec<(RoadClass, f32, Vec<[f32; 2]>)>,
    keep_out: &[[f32; 2]],
) -> Vec<Vec<[f32; 2]>> {
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
            // **Not paths, and no wider apart than a median** (`MEDIAN_GAP_M`,
            // the rule [`medians`] lays its floor by): two cycle ramps fifteen
            // metres apart were parallel and the same class, and their hull
            // swallowed the path between them (`audit`, 2026-09-29).
            if merged[j]
                || class == RoadClass::Path
                || other_class != class
                || (other_half - half).abs() > 0.01
                || ring_gap(&ring, other) > MEDIAN_GAP_M + 1.0
                || !parallel_and_near(&ring, other)
            {
                continue;
            }
            let mut both = ring.clone();
            both.extend_from_slice(other);
            let hull = convex_hull(&both);
            // **A hull may not take what neither half had**: a roofed stretch
            // or a street at grade between them. Two pieces of one slip road
            // meeting in a V were parallel enough to merge, and their hull
            // opened the lid at the V's point where a street crosses over
            // (`audit`, 2026-09-29).
            let hb = bbox_of_ring(&hull);
            let swallows = keep_out.iter().any(|&q| {
                point_in_bbox(q, &hb)
                    && point_in_ring(q, &hull)
                    && !point_in_ring(q, &ring)
                    && !point_in_ring(q, other)
            });
            if swallows {
                continue;
            }
            ring = hull;
            merged[j] = true;
        }
        out.push(ring);
    }
    out
}

/// The least distance between two rings' edges, metres.
fn ring_gap(a: &[[f32; 2]], b: &[[f32; 2]]) -> f32 {
    let to_edges = |p: [f32; 2], r: &[[f32; 2]]| {
        (0..r.len())
            .map(|i| {
                let (s, e) = (r[i], r[(i + 1) % r.len()]);
                project(s, e, p).map_or_else(
                    || {
                        (p[0] - s[0])
                            .hypot(p[1] - s[1])
                            .min((p[0] - e[0]).hypot(p[1] - e[1]))
                    },
                    |(d, _)| d,
                )
            })
            .fold(f32::MAX, f32::min)
    };
    a.iter()
        .map(|&p| to_edges(p, b))
        .chain(b.iter().map(|&p| to_edges(p, a)))
        .fold(f32::MAX, f32::min)
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
fn opening_rings(
    centre: &[[f32; 2]],
    roofed: &[bool],
    half: f32,
    reaches: &[[f32; 2]],
) -> Vec<Vec<[f32; 2]>> {
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
            let reach: Vec<[f32; 2]> = if reaches.len() == centre.len() {
                reaches[lo..=hi].to_vec()
            } else {
                vec![[half; 2]; hi + 1 - lo]
            };
            out.extend(mouth_ring(&centre[lo..=hi], &reach));
        }
    }
    out
}

/// One corridor as a closed ring, a little wider than the carriageway.
fn mouth_ring(run: &[[f32; 2]], reach: &[[f32; 2]]) -> Option<Vec<[f32; 2]>> {
    if run.len() < 2 || reach.len() != run.len() {
        return None;
    }
    // A little wider than the walls: the ground has to come away from *outside*
    // them, or a hairline of sheet is left standing on top of the wall. Per
    // segment, each end at its own vertex's reach, like the walls themselves.
    let mut left: Vec<[[f32; 2]; 2]> = Vec::with_capacity(run.len());
    let mut right: Vec<[[f32; 2]; 2]> = Vec::with_capacity(run.len());
    for i in 0..run.len() - 1 {
        let (a, b) = (run[i], run[i + 1]);
        let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dz).max(1e-6);
        let n = [-dz / len, dx / len];
        let at = |p: [f32; 2], d: f32| [p[0] + n[0] * d, p[1] + n[1] * d];
        left.push([at(a, reach[i][0] + 0.5), at(b, reach[i + 1][0] + 0.5)]);
        right.push([at(a, -reach[i][1] - 0.5), at(b, -reach[i + 1][1] - 0.5)]);
    }
    let mut ring: Vec<[f32; 2]> = Vec::with_capacity(run.len() * 2 + 1);
    for s in &left {
        ring.push(s[0]);
    }
    if let Some(last) = left.last() {
        ring.push(last[1]);
    }
    for s in right.iter().rev() {
        ring.push(s[1]);
    }
    if let Some(first) = right.first() {
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
mod tests {
    use super::*;
    use crate::road_texture::{road_class, StreetFlags};
    use crate::vector_tiles::kind_of;

    /// The ring tile's own ground size in metres. Derived from the tile's
    /// coordinates rather than written down: a hand-typed span puts every
    /// distance measured here out by a fraction of a per cent.
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

    fn fixtures() -> [(TileData, f32, &'static str); 2] {
        [
            (crate::testdata::groningen(), 1471.0, "groningen"),
            (
                crate::testdata::groningen_ring(),
                ring_tile_m(),
                "groningen_ring",
            ),
        ]
    }

    /// A straight run from `a` to `b`, densified the way a tile's runs are.
    fn run(level: i8, a: [f32; 2], b: [f32; 2], feature: usize) -> StreetRun {
        StreetRun {
            key: feature as u64,
            feature,
            level,
            class: RoadClass::Major,
            half_w: 5.0,
            points: road_levels::densify(&[a, b], 3.0),
            cut: [false; 2],
        }
    }

    /// A 200 m tunnel with 300 m of road out of each end.
    fn a_tunnel_and_its_roads() -> Vec<StreetRun> {
        vec![
            run(0, [-300.0, 0.0], [0.0, 0.0], 0),
            run(-1, [0.0, 0.0], [200.0, 0.0], 1),
            run(0, [200.0, 0.0], [500.0, 0.0], 2),
        ]
    }

    /// **A tunnel and its ramps are one structure, and the ramps are open.** With
    /// nothing measured the roads out of each end are graded down to the tunnel on
    /// the network — the same solve atlas runs — so the structure reaches out along
    /// them to where they have come back up, open to the sky; the tunnel itself is
    /// roofed end to end, with a headwall at each mouth.
    #[test]
    fn a_tunnel_and_its_ramps_are_one_structure() {
        let runs = a_tunnel_and_its_roads();
        let profiles = unmeasured_profiles(&runs);
        assert!(
            profiles.iter().all(Option::is_some),
            "a road out of the tunnel is not part of it"
        );
        let out = build_structures(&runs, &profiles, None);
        let bore = out
            .tunnels
            .iter()
            .find(|t| t.covered.iter().any(|&c| c))
            .expect("the tunnel is roofed");
        assert!(
            bore.covered.iter().all(|&c| c),
            "the tunnel has an open stretch"
        );
        assert!(
            bore.road_y
                .iter()
                .all(|&y| (y + LEVEL_DEPTH_M).abs() < 0.05),
            "the tunnel is not a level down: {:?}",
            bore.road_y
        );
        let ramps: Vec<&Tunnel> = out
            .tunnels
            .iter()
            .filter(|t| t.covered.iter().all(|&c| !c))
            .collect();
        assert_eq!(ramps.len(), 2, "expected a ramp at each end");
        for r in &ramps {
            let len: f32 = polyline_len(&r.centre);
            // 7 m at 6 % is 117 m, and the structure stops a metre short of the
            // top, plus the point beyond.
            assert!((95.0..125.0).contains(&len), "a ramp {len:.0} m long");
            let shallowest = r.road_y.iter().copied().fold(f32::MIN, f32::max);
            assert!(
                shallowest > -SUNK_M_TEST - 0.01,
                "a ramp stops {shallowest:.2} m down"
            );
        }
        assert_eq!(out.openings.len(), 2, "each ramp opens the ground over it");
        // One headwall per mouth: two quads whose top edge is at the land.
        let heads = out
            .meshes
            .iter()
            .filter(|m| m.group == SurfaceGroup::TunnelStructure)
            .flat_map(|m| m.normals.iter())
            .filter(|n| n[0].abs() > 0.99)
            .count();
        assert_eq!(
            heads,
            2 * 6,
            "expected a headwall over each of the two mouths"
        );
    }

    const SUNK_M_TEST: f32 = road_levels::SUNK_M;

    /// **The mouths are where the headwalls are**, facing out of the tunnel: one
    /// at each end of a tunnel with a ramp at each end.
    #[test]
    fn a_tunnel_has_a_mouth_at_each_end_facing_out() {
        let runs = a_tunnel_and_its_roads();
        let m = mouths(&runs, &unmeasured_profiles(&runs));
        assert_eq!(m.len(), 2, "{m:?}");
        for mouth in &m {
            // The tunnel runs x = 0..200; out of it is towards the nearer end.
            let expect = if mouth.at[0] < 100.0 { -1.0 } else { 1.0 };
            assert!((mouth.out[0] - expect).abs() < 1e-3, "{mouth:?}");
            assert!(mouth.road_y < mouth.land_y - 5.0);
        }
    }

    /// **The measured shape is drawn as measured.** A cutting the survey put nine
    /// metres under land two metres above the datum has its carriageway at −7 and
    /// its wall tops at +2 — the heights are final, and nothing lifts them.
    #[test]
    fn the_published_heights_are_the_drawn_ones() {
        let r = run(0, [0.0, 0.0], [120.0, 0.0], 0);
        let n = r.points.len();
        let profile = Profile {
            road: vec![-7.0; n],
            land: vec![2.0; n],
            walls: Vec::new(),
        };
        let out = build_structures(&[r], &[Some(profile)], None);
        let walls = out
            .meshes
            .iter()
            .find(|m| m.group == SurfaceGroup::TunnelStructure)
            .expect("walls");
        let (lo, hi) = walls
            .positions
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
        assert!(
            (lo + 7.0).abs() < 1e-4 && (hi - 2.0).abs() < 1e-4,
            "walls span {lo}..{hi}"
        );
        assert!(
            out.tunnels[0].covered.iter().all(|&c| !c),
            "a level-0 cutting was roofed"
        );
        assert_eq!(out.openings.len(), 1);
    }

    /// **Two halves of one road in a cutting share its floor.** Twelve metres
    /// apart, five metres down: the walls stand on the outside of each, and
    /// between them there is a floor under the road and a barrier no taller than
    /// a barrier — not two walls the depth of the cutting with a void between, as
    /// the DUO cutting was drawn until 2026-09-28.
    #[test]
    fn a_dual_carriageway_in_a_cutting_shares_its_floor() {
        let runs = [
            run(0, [0.0, 0.0], [120.0, 0.0], 0),
            run(0, [0.0, 12.0], [120.0, 12.0], 1),
        ];
        let profiles: Vec<Option<Profile>> = runs
            .iter()
            .map(|r| {
                Some(Profile {
                    road: vec![-5.0; r.points.len()],
                    land: vec![0.0; r.points.len()],
                    walls: Vec::new(),
                })
            })
            .collect();
        let out = build_structures(&runs, &profiles, None);
        let shell = out
            .meshes
            .iter()
            .find(|m| m.group == SurfaceGroup::TunnelStructure)
            .expect("walls");
        let between = shell
            .positions
            .iter()
            .filter(|p| p[2] > 5.5 && p[2] < 6.5)
            .map(|p| p[1])
            .fold(f32::MIN, f32::max);
        assert!(
            between <= -5.0 + BARRIER_M + 1e-3,
            "something {between} m high stands in the median"
        );
        assert!(
            shell.normals.iter().any(|n| n[1] > 0.99),
            "no floor was laid across the median"
        );
        let outer = shell
            .positions
            .iter()
            .filter(|p| p[2] < -4.9 || p[2] > 16.9)
            .map(|p| p[1]);
        assert!(
            outer.fold(f32::MIN, f32::max) >= -1e-3,
            "the outer walls do not reach the land"
        );
    }

    /// **The walls stand where the survey put them.** A ramp three metres down
    /// whose trench is sixteen metres wide on one side of its line and a bank on
    /// the other: a wall eight metres out on the left with floor between it and
    /// the carriageway, nothing standing on the right, and the ground opened out
    /// to the wall — not a slot the width of the road (2026-09-28).
    #[test]
    fn the_walls_stand_where_the_survey_put_them() {
        let r = run(0, [0.0, 0.0], [60.0, 0.0], 0);
        let n = r.points.len();
        let profile = Profile {
            road: vec![-3.0; n],
            land: vec![0.0; n],
            walls: vec![[8.0, 0.0]; n],
        };
        let out = build_structures(&[r], &[Some(profile)], None);
        let shell = out
            .meshes
            .iter()
            .find(|m| m.group == SurfaceGroup::TunnelStructure)
            .expect("a wall");
        // Left of a run heading east is +y (south) in this frame.
        assert!(
            shell
                .positions
                .iter()
                .any(|p| (p[2] - 8.0).abs() < 1e-3 && p[1] > -1e-3),
            "no wall at eight metres reaching the land"
        );
        assert!(
            shell.positions.iter().all(|p| p[2] > -5.0 - 1e-3),
            "something stands on the side that is a bank"
        );
        assert!(
            shell
                .positions
                .iter()
                .zip(&shell.normals)
                .any(|(p, n)| n[1] > 0.99 && (p[2] - 8.0).abs() < 1e-3),
            "no floor out to the wall"
        );
        let ring = &out.openings[0];
        let (lo, hi) = ring.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p[1]), hi.max(p[1]))
        });
        assert!(
            (hi - 8.5).abs() < 1e-3,
            "opening reaches {hi}, not past the wall"
        );
        assert!(
            (lo + 5.5).abs() < 1e-3,
            "opening reaches {lo} on the bank side"
        );
    }

    /// **A shallow underpass is roofed under the street it passes below.** A
    /// path 2.5 m under its land has no room for its headroom and a slab, and
    /// its open stretch cut a slot through the road over it (2026-09-29). Under
    /// the road it is roofed, with the ceiling held under the land, and the
    /// ground is opened only either side of it.
    #[test]
    fn a_shallow_underpass_is_roofed_under_the_road_it_passes_below() {
        let mut path = run(-1, [0.0, 0.0], [60.0, 0.0], 0);
        path.class = RoadClass::Path;
        path.half_w = 1.5;
        let road = run(0, [30.0, -40.0], [30.0, 40.0], 1);
        let n = path.points.len();
        let profiles = vec![
            Some(Profile {
                road: vec![-2.5; n],
                land: vec![0.0; n],
                walls: Vec::new(),
            }),
            None,
        ];
        let out = build_structures(&[path, road], &profiles, None);
        let t = &out.tunnels[0];
        let under = (0..t.centre.len() - 1)
            .find(|&k| t.centre[k][0] <= 30.0 && t.centre[k + 1][0] >= 30.0)
            .expect("a segment under the road");
        assert!(
            t.covered[under] && t.covered[under + 1],
            "not roofed under the road"
        );
        let shell = out
            .meshes
            .iter()
            .find(|m| m.group == SurfaceGroup::TunnelStructure)
            .expect("a shell");
        assert!(
            shell
                .positions
                .iter()
                .filter(|p| (p[0] - 30.0).abs() < 6.0)
                .all(|p| p[1] <= -ROOF_SLAB_M + 1e-3),
            "something under the road stands above the slab under the land"
        );
        for ring in &out.openings {
            assert!(
                !ring.iter().any(|p| (p[0] - 30.0).abs() < 4.0) || ring.len() < 3,
                "the ground was opened under the road"
            );
        }
    }

    /// **A tunnel under a span is open.** Atlas measures walls only for a
    /// stretch open to the sky, which is how a path OSM tags as a tunnel beneath
    /// a bridge reaches the client: deep under its land, and never to be given a
    /// lid (2026-09-28).
    #[test]
    fn a_tunnel_under_a_span_is_not_roofed() {
        let bore = run(-1, [0.0, 0.0], [60.0, 0.0], 0);
        let n = bore.points.len();
        let profile = Profile {
            road: vec![-7.0; n],
            land: vec![0.0; n],
            walls: vec![[9.0, 9.0]; n],
        };
        let out = build_structures(&[bore], &[Some(profile)], None);
        assert!(
            out.tunnels.iter().all(|t| t.covered.iter().all(|c| !c)),
            "a stretch atlas measured walls for was roofed"
        );
        assert!(!out.openings.is_empty(), "and its ground was not opened");
    }

    /// **A trench wider than the bore it runs into is closed where they meet**,
    /// from the bore's walls out to its own, or the mouth shows the void behind
    /// the earth the opening took away.
    #[test]
    fn a_wide_trench_meeting_a_narrow_bore_is_closed() {
        let ramp = run(0, [0.0, 0.0], [60.0, 0.0], 0);
        let bore = run(-1, [60.0, 0.0], [160.0, 0.0], 1);
        let profiles = vec![
            Some(Profile {
                road: vec![-4.0; ramp.points.len()],
                land: vec![0.0; ramp.points.len()],
                walls: vec![[8.0, 8.0]; ramp.points.len()],
            }),
            Some(Profile {
                road: vec![-7.0; bore.points.len()],
                land: vec![0.0; bore.points.len()],
                walls: Vec::new(),
            }),
        ];
        let out = build_structures(&[ramp, bore], &profiles, None);
        let shell = out
            .meshes
            .iter()
            .find(|m| m.group == SurfaceGroup::TunnelStructure)
            .expect("walls");
        for side in [1.0f32, -1.0] {
            assert!(
                shell.positions.iter().zip(&shell.normals).any(|(p, n)| {
                    (p[0] - 60.0).abs() < 1e-3 && (p[2] * side - 8.0).abs() < 1e-3 && n[0] < -0.99
                }),
                "no wing on the {side} side where the ramp meets the bore"
            );
        }
    }

    /// The most low roofs either fixture had when [`audit`] was written
    /// (2026-09-29): a ceiling on how far this can drift unnoticed.
    const LOW_ROOFS_MAX: usize = 6;

    /// **The fixtures cut no street and step into nothing** — on flat land, as a
    /// tile with no atlas behind it is built. Low roofs are reported rather than
    /// refused: on flat land a path junction at grade seven metres before a
    /// carriageway leaves the underpass under it less than a metre of headroom,
    /// and a low roof is the honest drawing of that (`MIN_HEADROOM_M`).
    #[test]
    fn the_fixtures_pass_the_audit() {
        let mut tally = Vec::new();
        for (tile, tile_m, name) in fixtures() {
            let runs = road_levels::street_runs(&tile, tile_m);
            let found = audit(&runs, &unmeasured_profiles(&runs));
            for f in &found {
                let street = runs.iter().find(|r| {
                    r.points
                        .iter()
                        .any(|p| (p[0] - f.at[0]).hypot(p[1] - f.at[1]) < 0.01)
                });
                println!(
                    "{name}: {:?} at {:?} on {:?}",
                    f.flaw,
                    f.at,
                    street.map(|r| (
                        r.level,
                        r.class,
                        r.half_w,
                        r.points[0],
                        r.points[r.points.len() - 1]
                    ))
                );
            }
            let broken = found
                .iter()
                .filter(|f| f.flaw != Flaw::CeilingOutOfPlace)
                .count();
            tally.push((name, broken, found.len() - broken));
        }
        println!("{tally:?}");
        for (name, broken, low) in tally {
            assert_eq!(
                broken, 0,
                "{name}: {broken} streets cut or steps into nothing"
            );
            assert!(
                low <= LOW_ROOFS_MAX,
                "{name}: {low} low roofs, more than {LOW_ROOFS_MAX}"
            );
        }
    }

    /// **A roofed deck keeps its headroom under the land.**
    #[test]
    fn a_roofed_deck_keeps_its_headroom() {
        for (tile, tile_m, name) in fixtures() {
            let out = build_tunnels(&tile, tile_m, None);
            for t in &out.tunnels {
                for (i, covered) in t.covered.iter().enumerate() {
                    if *covered {
                        let ceiling = t.ceiling(i);
                        assert!(
                            ceiling <= t.land_y[i] - ROOF_SLAB_M + 0.01
                                && ceiling >= t.road_y[i] + MIN_HEADROOM_M - 0.01,
                            "{name}: a lid at {ceiling} m is not under the land at {} m",
                            t.land_y[i]
                        );
                    }
                }
            }
        }
    }

    /// **A tunnel's carriageway must face up**, or the road vanishes at the
    /// portal. Same rule and same test as `vector_tiles`' `ribbon_faces_up`: in
    /// this frame — x east, y south — an up-facing triangle has a non-positive
    /// cross product.
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

    /// **Every face of a structure is wound toward the side its normal says**,
    /// which for the roof and the walls is the *inside*. The material culls back
    /// faces, so a winding that disagrees with the normal is a surface that is not
    /// there from the only side anybody sees it from — from inside a lid the
    /// tunnel once had no ceiling and no sides (2026-09-23).
    #[test]
    fn every_tunnel_face_is_wound_toward_its_normal() {
        let mut checked = [0usize; 2];
        for (tile, tile_m, _) in fixtures() {
            let out = build_tunnels(&tile, tile_m, None);
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
                        "a {:?} triangle is wound away from its normal {n:?}: {a:?} {b:?} {c:?}",
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
    /// wall carried on to the land stood out of the Oude Winschoterdiep as two
    /// long slivers along the lid under it (2026-09-23).
    #[test]
    fn no_wall_rises_through_a_lid() {
        let out = build_tunnels(&crate::testdata::groningen_ring(), ring_tile_m(), None);
        let mut checked = 0usize;
        for m in out
            .meshes
            .iter()
            .filter(|m| m.group == SurfaceGroup::TunnelStructure)
        {
            for p in &m.positions {
                for t in out.tunnels.iter().filter(|t| t.half_w >= 3.0) {
                    for k in 1..t.centre.len().saturating_sub(1) {
                        if !(t.covered[k - 1] && t.covered[k] && t.covered[k + 1]) {
                            continue;
                        }
                        let c = t.centre[k];
                        if (p[0] - c[0]).hypot(p[2] - c[1]) > t.half_w + 0.05 {
                            continue;
                        }
                        let ceiling = t.ceiling(k);
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

    /// **No open structure ends below its land with nothing beyond it.** Every
    /// open end of every structure either comes back up to within a walled road's
    /// depth of its land, or is cut by the tile border, or is met by another
    /// structure at the same point and the same height. A step is none of those —
    /// it is the carriageway falling off a cliff, which is what the ring did six
    /// times until 2026-09-22, what a slip road out of a portal did at grade beside
    /// a trench seven metres deep (2026-09-23), and what a cycle path's ramp did
    /// where it ran into the side of a street that had no vertex there
    /// (`road_levels::TEE_M`). A roofed end with nothing beyond is a tunnel into
    /// something the map does not draw — a garage under a building — and is
    /// closed by its headwall.
    #[test]
    fn no_open_structure_ends_below_its_land() {
        for (tile, tile_m, name) in fixtures() {
            let runs = road_levels::street_runs(&tile, tile_m);
            let plans = plan(&runs, &unmeasured_profiles(&runs));
            let ends: Vec<([f32; 2], f32, f32, bool, bool)> = plans
                .iter()
                .flat_map(|t| {
                    let n = t.centre.len() - 1;
                    [
                        (
                            t.centre[0],
                            t.road_y[0],
                            t.land_y[0],
                            t.roofed[0] || t.cut[0],
                            t.dead[0],
                        ),
                        (
                            t.centre[n],
                            t.road_y[n],
                            t.land_y[n],
                            t.roofed[n] || t.cut[1],
                            t.dead[1],
                        ),
                    ]
                })
                .collect();
            let (mut deep, mut closed) = (0usize, 0usize);
            for (i, &(p, y, g, roofed_or_cut, dead)) in ends.iter().enumerate() {
                if y > g - SUNK_M_TEST - 0.01 || roofed_or_cut {
                    continue;
                }
                if dead {
                    closed += 1;
                    continue;
                }
                deep += 1;
                let met = ends.iter().enumerate().any(|(j, &(q, oy, ..))| {
                    j != i && (q[0] - p[0]).hypot(q[1] - p[1]) < 0.02 && (oy - y).abs() < 0.05
                });
                assert!(met, "{name}: a structure ends {:.1} m under its land at {p:?} with nothing beyond it", g - y);
            }
            println!("{name}: {deep} deep open ends met, {closed} closed dead ends");
        }
    }

    /// **A road that ends in nothing below its land is closed by a wall** facing
    /// back along it: a cutting stub, sixty metres long and seven deep, with no
    /// other street anywhere near either end.
    #[test]
    fn a_dead_end_below_the_land_is_walled() {
        let r = run(0, [0.0, 0.0], [60.0, 0.0], 0);
        let n = r.points.len();
        let profile = Profile {
            road: vec![-7.0; n],
            land: vec![0.0; n],
            walls: Vec::new(),
        };
        let out = build_structures(&[r], &[Some(profile)], None);
        let ends = out
            .meshes
            .iter()
            .filter(|m| m.group == SurfaceGroup::TunnelStructure)
            .flat_map(|m| m.normals.iter())
            .filter(|n| n[0].abs() > 0.99)
            .count();
        assert_eq!(ends, 2 * 6, "expected an end wall at each end of the stub");
    }

    /// **A building in a covered tunnel's old trench is the tunnel's, and one on
    /// the lid is not.** The rule needs both halves: the place, and the ground
    /// under it being well below the land around.
    #[test]
    fn a_building_in_a_covered_trench_is_told_from_one_on_the_lid() {
        let out = build_tunnels(&crate::testdata::groningen_ring(), ring_tile_m(), None);
        let t = out
            .tunnels
            .iter()
            .find(|t| t.half_w >= 3.0 && t.covered.iter().all(|c| *c) && t.centre.len() >= 3)
            .expect("the ring has a lid");
        let (a, b) = (t.centre[0], t.centre[t.centre.len() - 1]);
        let over = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let over = t
            .centre
            .iter()
            .copied()
            .min_by(|p, q| {
                (p[0] - over[0])
                    .hypot(p[1] - over[1])
                    .total_cmp(&(q[0] - over[0]).hypot(q[1] - over[1]))
            })
            .unwrap();
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
        let beside = [over[0] + 60.0, over[1] + 60.0];
        assert!(!stands_in_a_covered_trench(
            &out.tunnels,
            &out.openings,
            beside,
            7.0
        ));
    }

    /// **The survey's own tunnel walls are recognised, and a wall elsewhere is
    /// not.** Measured on the ring tile's BGT (2026-09-23), in this tile's metres.
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
        let elsewhere = [[691.0f32, 656.0], [779.0, 578.0]];
        assert!(!wall_of_a_sunken_road(
            &out.tunnels,
            &out.openings,
            &elsewhere
        ));
    }

    /// **No crown hangs over an open trench** once the caller's filter has run.
    #[test]
    fn no_tree_crown_reaches_into_an_opening() {
        let tile_m = ring_tile_m();
        let tile = crate::testdata::groningen_ring();
        let out = build_tunnels(&tile, tile_m, None);
        let mut trees = crate::scatter::scatter_vegetation(&tile, tile_m, 7).expect("scatters");
        // The trees this filter used to find near an opening here stood by
        // "tunnels" less than a metre under flat land, which are at grade now
        // (`road_levels::structures`); the check that matters is below.
        assert!(
            !out.openings.is_empty(),
            "the fixture cuts no opening to test against"
        );
        trees.retain(|t| !near_an_opening(&out.openings, [t.x, t.z], t.radius * 0.8));
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

    /// **A sunken road under water is covered.** A road can be open to the sky
    /// or under a canal, never both — the phone once showed the ring's carriageway
    /// crossing the Oude Winschoterdiep at water level (2026-09-23).
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
        for (tile, tile_m, name) in fixtures() {
            let pools = water(&tile, tile_m);
            let out = build_tunnels(&tile, tile_m, None);
            for t in &out.tunnels {
                for i in 0..t.centre.len().saturating_sub(1) {
                    let (a, b) = (t.centre[i], t.centre[i + 1]);
                    let deep =
                        (t.road_y[i] - t.land_y[i]).min(t.road_y[i + 1] - t.land_y[i + 1]) < -1.0;
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
                                "{name}: a sunken road at {p:?} runs open under water"
                            );
                        }
                    }
                }
            }
        }
        assert!(
            covered_under_water > 0,
            "no tunnel passes under water in either fixture"
        );
    }

    /// **The ground may not be cut away over a roofed stretch.** The hole and the
    /// absent ceiling are decided by one test, so a roofed segment must not lie
    /// inside any opening — and `merge_parallel_openings`, which replaces two
    /// rings with their hull, is what could break that.
    /// How many openings each fixture cuts, and their total area.
    #[test]
    #[ignore = "diagnostic"]
    fn print_opening_area() {
        for (tile, tile_m, name) in fixtures() {
            let out = build_tunnels(&tile, tile_m, None);
            let area: f32 = out
                .openings
                .iter()
                .map(|r| {
                    let mut a = 0.0;
                    for i in 0..r.len() {
                        let (p, q) = (r[i], r[(i + 1) % r.len()]);
                        a += p[0] * q[1] - q[0] * p[1];
                    }
                    (a * 0.5).abs()
                })
                .sum();
            println!("OPEN {name} {} rings {area:.0} m2", out.openings.len());
        }
    }

    /// Every ring-fixture structure within 40 m of `CARTO_AT` = "x,z", vertex by
    /// vertex: road, land, ceiling and roof — for reading a failing geometry
    /// test's coordinates back to the structure it is about.
    #[test]
    #[ignore = "diagnostic"]
    fn print_tunnels_near_a_point() {
        let (tile, tile_m) = if std::env::var("CARTO_FIXTURE").as_deref() == Ok("groningen") {
            (crate::testdata::groningen(), 1471.0)
        } else {
            (crate::testdata::groningen_ring(), ring_tile_m())
        };
        let out = build_tunnels(&tile, tile_m, None);
        let runs = road_levels::street_runs(&tile, tile_m);
        let q: Vec<f32> = std::env::var("CARTO_AT")
            .unwrap()
            .split(',')
            .map(|v| v.parse().unwrap())
            .collect();
        for t in &out.tunnels {
            if t.centre
                .iter()
                .any(|p| (p[0] - q[0]).hypot(p[1] - q[1]) < 40.0)
            {
                println!("half {} clearance {}", t.half_w, t.clearance);
                for (k, p) in t.centre.iter().enumerate() {
                    println!(
                        "  {:.1},{:.1} road {:.2} land {:.2} ceil {:.2} {}",
                        p[0],
                        p[1],
                        t.road_y[k],
                        t.land_y[k],
                        t.ceiling(k),
                        if t.covered[k] { "R" } else { "." }
                    );
                }
            }
        }
        for r in &runs {
            if r.points
                .iter()
                .any(|p| (p[0] - q[0]).hypot(p[1] - q[1]) < 15.0)
            {
                println!(
                    "street level {} {:?} half {} {:?} .. {:?}",
                    r.level,
                    r.class,
                    r.half_w,
                    r.points[0],
                    r.points[r.points.len() - 1]
                );
            }
        }
    }

    #[test]
    fn no_opening_swallows_a_roofed_stretch() {
        for (tile, tile_m, name) in fixtures() {
            let out = build_tunnels(&tile, tile_m, None);
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
                                "{name}: the ground is cut away over a lid at {p:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// **The ring's cuttings between the lids are open, and the lids are not**,
    /// and neither is a four-metre seam in the tagging ([`MIN_OPEN_M`]).
    #[test]
    fn the_cuttings_are_open_and_the_lids_are_not() {
        let out = build_tunnels(&crate::testdata::groningen_ring(), ring_tile_m(), None);
        let roads: Vec<&Tunnel> = out.tunnels.iter().filter(|t| t.half_w >= 4.0).collect();
        let (mut open, mut lidded) = (0, 0);
        for t in &roads {
            if t.covered.iter().all(|c| !c) {
                let len = polyline_len(&t.centre);
                open += 1;
                let deep = t.road_y.iter().zip(&t.land_y).any(|(y, g)| y - g < -3.0);
                assert!(
                    deep || len >= MIN_OPEN_M,
                    "a {len:.0} m shallow stretch was opened"
                );
            } else {
                lidded += 1;
            }
        }
        assert!(open >= 4, "expected the ring's open cuttings, got {open}");
        assert!(lidded >= 6, "expected the ring's lids, got {lidded}");
    }

    /// **Each tunnel keeps its own class**, so a cell with a cycle underpass and a
    /// road tunnel draws gravel under one and tarmac with lane markings under the
    /// other. Over Groningen's ring a single class for the cell drew the motorway
    /// in a footpath's gravel in the next cell (2026-09-20).
    #[test]
    fn each_tunnel_keeps_its_own_class() {
        let tile = crate::testdata::groningen();
        let out = build_tunnels(&tile, 1471.0, None);
        let meta = tile.layer("streets").expect("streets");
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
        assert!(
            drawn.len() >= wanted.len().min(2),
            "tunnels of {wanted:?} are drawn as {drawn:?} — the decks were merged onto one class"
        );
    }

    #[test]
    fn the_deck_is_found_under_the_carriageway_and_not_beside_it() {
        let tunnel = Tunnel {
            centre: vec![[0.0, 0.0], [100.0, 0.0]],
            road_y: vec![-7.0, -7.0],
            land_y: vec![0.0, 0.0],
            half_w: 6.0,
            clearance: CLEARANCE_M,
            covered: vec![true; 2],
        };
        assert_eq!(tunnel.road_under([50.0, 0.0]), Some(-7.0));
        assert_eq!(tunnel.road_under([50.0, 5.9]), Some(-7.0));
        assert!(tunnel.road_under([50.0, 8.0]).is_none());
        assert!(tunnel.road_under([-20.0, 0.0]).is_none());
        assert_eq!(tunnel.depth_near([50.0, 0.0], 0.0), Some(-7.0));
    }

    #[test]
    fn the_hole_in_the_ground_covers_exactly_the_stretch_with_no_roof() {
        let centre: Vec<[f32; 2]> = (0..=60).map(|i| [i as f32 * 10.0, 0.0]).collect();
        let roofed: Vec<bool> = (0..=60).map(|i| (12..=48).contains(&i)).collect();
        let rings = opening_rings(&centre, &roofed, 6.0, &[]);
        assert_eq!(
            rings.len(),
            2,
            "an open stretch at each end should open two holes"
        );
        for ring in &rings {
            assert!(ring.len() >= 4);
            assert_eq!(ring.first(), ring.last(), "the hole is not a closed ring");
        }
        assert!(opening_rings(&centre[..2], &[true, true], 6.0, &[]).is_empty());
    }

    /// What the live tiles actually carry, read off the Groningen fixture: the
    /// one claim about the data rather than about the geometry that the module
    /// rests on.
    #[test]
    fn the_tiles_say_which_streets_are_tunnels() {
        let tile = crate::testdata::groningen();
        let meta = tile.layer("streets").expect("a streets layer");
        let features = tile.features(meta);
        let flagged = features.iter().filter(|f| flag_of(f, "tunnel")).count();
        let below = features.iter().filter(|f| underground(f)).count();
        assert!(flagged > 0, "no street in the fixture is flagged a tunnel");
        assert_eq!(
            below, flagged,
            "`underground` is the bare flag — the tiles carry no `layer`"
        );
        // **No `layer` anywhere in the layer**: if a tileset ever starts carrying
        // one, this fails and the depth convention has something better to read.
        assert!(
            features
                .iter()
                .all(|f| crate::vector_tiles::layer_of(f) == 0),
            "a street carries a `layer` now"
        );
        let passages = building_passages(&tile, &features);
        assert!(
            passages.len() < flagged,
            "every flagged way was called a building passage"
        );
    }

    /// The cut must actually take ground away: a no-op here looks exactly like a
    /// feature that was never built.
    #[test]
    fn the_cut_removes_the_ground_over_an_opening() {
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
        let opening = vec![
            [40.0, -10.0],
            [60.0, -10.0],
            [60.0, 110.0],
            [40.0, 110.0],
            [40.0, -10.0],
        ];
        // **Both ways round**: an opening's winding follows its tunnel's direction.
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
        let opening = vec![
            [-10.0, -100.0],
            [10.0, -100.0],
            [10.0, 100.0],
            [-10.0, 100.0],
            [-10.0, -100.0],
        ];
        assert!(cut_sheet(&mut sheet, &[opening]));
        let gone = before - mesh_area(&sheet);
        assert!((gone - 20.0 * 184.0).abs() < 100.0, "{gone:.0} m² went");
        for (p, uv) in sheet.positions.iter().zip(sheet.uvs.iter()) {
            let want = [(p[0] + 92.0) / 184.0, (p[2] + 92.0) / 184.0];
            assert!(
                (uv[0] - want[0]).abs() < 1e-3 && (uv[1] - want[1]).abs() < 1e-3,
                "vertex at {p:?} carries uv {uv:?}, not {want:?}"
            );
        }
    }

    /// **Ground over a lid is whole and ground over the open ramp is not**, and
    /// the fill fades out with distance rather than stopping at a crease.
    #[test]
    fn the_fill_covers_the_lid_and_stops_at_the_ramp() {
        let tunnel = Tunnel {
            centre: vec![[0.0, 0.0], [50.0, 0.0], [100.0, 0.0]],
            road_y: vec![0.0, -0.5, -7.0],
            land_y: vec![0.0, 0.0, 0.0],
            half_w: 6.0,
            clearance: CLEARANCE_M,
            covered: vec![false, true, true],
        };
        let taper = 30.0;
        assert_eq!(tunnel.fill_weight([75.0, 0.0], taper), 1.0);
        assert_eq!(tunnel.fill_weight([75.0, 5.0], taper), 1.0);
        assert_eq!(
            tunnel.fill_weight([10.0, 0.0], taper),
            0.0,
            "the shallow ramp was filled in"
        );
        assert_eq!(tunnel.fill_weight([75.0, 400.0], taper), 0.0);
        let near = tunnel.fill_weight([75.0, 12.0], taper);
        let far = tunnel.fill_weight([75.0, 30.0], taper);
        assert!(
            (0.0..1.0).contains(&near) && near > far,
            "{near} then {far}"
        );
        assert_eq!(tunnel.land_near([75.0, 20.0]).1, 0.0);
    }

    /// **The fill is part of the surface the refinement resolves**: over a lid
    /// the survey's nine-metre trench is put back to the land, and the lift is
    /// big enough under a chord across the corridor for the crease rule to see.
    #[test]
    fn the_lid_is_visible_to_the_crease_rule() {
        struct Trench;
        impl GroundField for Trench {
            fn min_ground_edge_m(&self) -> f32 {
                crate::terrain_refine::MIN_GROUND_EDGE_M
            }
            fn at(&self, _x: f32, _z: f32) -> f32 {
                -9.0
            }
        }
        let t = Tunnel {
            centre: vec![[0.0, 0.0], [100.0, 0.0], [200.0, 0.0]],
            road_y: vec![-7.0; 3],
            land_y: vec![0.0; 3],
            half_w: 5.0,
            clearance: CLEARANCE_M,
            covered: vec![true; 3],
        };
        let around = |_x: f32, _z: f32| 0.0;
        let weight = |p: [f32; 2]| t.fill_weight(p, DATUM_M);
        let field = Lidded {
            ground: &Trench,
            around: &around,
            weight: &weight,
        };
        assert!(
            field.at(100.0, 0.0).abs() < 1e-3,
            "the ground over a lid stayed down"
        );
        assert!((field.at(100.0, 5.0 + DATUM_M + 1.0) + 9.0).abs() < 1e-3);
        let chord = f32::midpoint(field.at(100.0, -50.0), field.at(100.0, 50.0));
        let sag = (field.at(100.0, 0.0) - chord).abs();
        assert!(
            sag > crate::terrain_refine::CREASE_SAG_M,
            "the lid sags only {sag} m"
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

    /// **Nothing a tunnel emits may ask the depth buffer for more than the road
    /// band holds**, and the deck asks for the ground rung — "inside the band"
    /// would still be satisfied by the bridge rung, which would put a tunnel over
    /// a flyover.
    #[test]
    fn a_tunnel_deck_draws_on_the_ground_rung() {
        let out = build_tunnels(&crate::testdata::groningen(), 1471.0, None);
        assert!(!out.meshes.is_empty(), "fixture grew no tunnels");
        let ceiling = crate::vector_tiles::ROAD_DEPTH_RUNGS as f32;
        for m in &out.meshes {
            for &r in &m.rungs {
                assert!(
                    (0.0..ceiling).contains(&r),
                    "{:?} emitted rung {r}",
                    m.group
                );
            }
            if matches!(m.group, SurfaceGroup::TunnelRoad(_)) {
                assert!(m.rungs.iter().all(|&r| r == road_depth_rung(false)));
            }
        }
    }

    /// A side view of each structure in a tile, in text — how far down the road
    /// goes, over what distance, under how much land and which part has a lid.
    /// `CARTO_TUNNEL_TILE=/path/to.mvt` to read a live tile instead.
    #[test]
    #[ignore = "diagnostic"]
    fn dump_the_fixture_structures() {
        let owned = std::env::var("CARTO_TUNNEL_TILE")
            .ok()
            .map(|p| std::fs::read(p).expect("tile reads"));
        let tile = match &owned {
            Some(b) => TileData::parse(b).expect("tile parses"),
            None => crate::testdata::groningen_ring(),
        };
        let out = build_tunnels(&tile, ring_tile_m(), None);
        println!(
            "structures={} openings={}",
            out.tunnels.len(),
            out.openings.len()
        );
        for (i, t) in out.tunnels.iter().enumerate() {
            println!(
                "structure {i}: half_w={:.1} len={:.0} m from {:?}",
                t.half_w,
                polyline_len(&t.centre),
                t.centre[0]
            );
            let mut along = 0.0f32;
            for k in 0..t.centre.len() {
                if k > 0 {
                    along += (t.centre[k][0] - t.centre[k - 1][0])
                        .hypot(t.centre[k][1] - t.centre[k - 1][1]);
                }
                println!(
                    "  {along:6.0} m  road {:6.2}  land {:6.2}  {}",
                    t.road_y[k],
                    t.land_y[k],
                    if t.covered[k] { "lid" } else { "open" }
                );
            }
        }
    }
}
