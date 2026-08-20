//! Routing over the vector-tile street network (ADR-037).
//!
//! The MVT basemap (ADR-035) does not just carry pixels — its `streets` layer is
//! the road network as geometry, with the attributes a router needs (`kind`,
//! `oneway`, `bicycle`, `surface`). This module turns those tiles into a graph and
//! runs A* over it, so a route is computed from the data we already fetch and
//! cache. No routing server, no second data source, and it works offline for any
//! area whose tiles are in the on-disk cache.
//!
//! Everything here is pure CPU and free of Bevy — the Bevy side (streaming the
//! tiles, drawing the line, the planner UI) lives in `systems::navigation`.
//!
//! **Why tile seams don't break connectivity.** MVT coordinates are integers on a
//! per-tile grid of `extent` units, and every tile at a zoom shares one global
//! lattice: `global = tile_index · extent + local`. A way crossing a tile border is
//! clipped into both tiles with a buffer, so the *same* OSM vertex appears in both —
//! and because the lattices align, it quantises to the *same* global integer in
//! both. Node identity is therefore exact, not a distance tolerance
//! ([`Node`] is that integer pair), which is what makes a graph stitched from
//! independently-encoded tiles routable at all.
//!
//! **What the graph is not.** Shortbread merges ways by attribute, so a "feature"
//! is a `MultiLineString` of everything sharing (`kind`, `oneway`, `surface`, …) —
//! there is no way id and no junction table. We build a node per *vertex* instead
//! of per junction: two ways that cross at an OSM node both carry that node's
//! coordinate, so a shared vertex is a shared graph node and the junction exists
//! implicitly. The cost is a graph with many degree-2 nodes, which A* walks through
//! cheaply and [`Route`] collapses again when it writes the turn list.
//!
//! **Why [`RoadGraph::finish`] exists.** Shared vertices are not enough on their
//! own. The tiles are generalised *per feature*, so where a side street tees into a
//! main road the tee is an interior vertex of the main road's merged line — and
//! simplification is free to drop it. The side street's endpoint then lies exactly
//! *on* the main road's segment while being a vertex of neither. Measured on the
//! Groningen fixture that is 14% of all nodes, and it shatters the network: the
//! largest connected car component covered 58% of the usable nodes before noding
//! and 92% after (walking: 60% → 96%). `finish` therefore runs a noding pass — split every segment at
//! the nodes lying on it — before any edge is emitted. Skipping it does not fail
//! loudly; it just returns [`RouteError::NoPath`] for perfectly ordinary routes,
//! which is why the pass is not optional and the tests pin the component share.
//!
//! **Why the noding pass is guarded.** It invents a junction wherever a node lies
//! on a segment, and it has no way ids to check the invention against — so left to
//! itself it also joins a viaduct to the street it crosses, and a car leaves the
//! underpass, climbs onto the motorway, and nothing in the returned route says it
//! happened. Two rules decide, and the first does most of the work:
//!
//! * **Only a dangle may split.** The tee this pass exists to repair is a line that
//!   *stops dead* on another line, its shared vertex generalised away. A line that
//!   carries on past the contact was never touching it. Marking each line's own two
//!   ends is enough to tell those apart, it needs nothing the tiles do not already
//!   say — and it is the only rule here that separates two viaducts crossing each
//!   other at an interchange, where no attribute in the data distinguishes them.
//! * **…and only at its own grade.** A slip road really does end on the carriageway
//!   it joins, so being a dangle is not sufficient: a bridge's abutment is a dangle
//!   sitting directly over the street it lands beside. Every segment carries a
//!   [`level`](RawSeg::level) — OSM's `layer` where the tile has one, else the
//!   `bridge` / `tunnel` flags — and each node the levels of the segments that end
//!   at it; a split needs a level in common.
//!
//! Refusing a split can never sever anything the tiles connected by a shared vertex
//! — that connectivity is node identity and runs underneath this pass — so both
//! guards are one-directional: they remove junctions that were never there and
//! cannot remove one that was.
//!
//! The residue it does **not** fix is a bridge vertex that quantises onto a street
//! vertex, which fuses them into one node before any of this runs. Splitting node
//! identity by level would fix that and break something worse: a bridge meets the
//! ground at its abutments through exactly such a shared vertex, so a level-keyed
//! node would disconnect every bridge in the network from both its ends.

use std::collections::{BinaryHeap, HashMap};

use geo_types::Geometry;
use mvt_reader::feature::{Feature, Value};

use cartopolis_core::world::tile_to_meters;

use crate::vector_tiles::{TileData, MAX_SOURCE_ZOOM, TILE_BUFFER_FRAC};
use cartopolis_core::lat_lng_to_tile_frac;

/// Zoom the graph is built at. Shortbread carries the complete street network at
/// its top zoom and nothing finer exists, so this is both the best and the only
/// choice — and it matches the zoom `map_geometry` already streams, so the bytes
/// are usually in the on-disk cache before a route is ever asked for.
pub const ROUTING_ZOOM: u32 = MAX_SOURCE_ZOOM;

/// The lattice all tile-local coordinates are normalised to. Shortbread emits
/// `extent = 4096`; a source that disagrees is rescaled on the way in so the global
/// lattice — and hence node identity across seams — stays consistent.
pub const EXTENT: f64 = 4096.0;

/// Ceiling on how many tiles one route may pull in. A source tile is ~500 KB, so
/// this is what separates "a moment on a cold cache" from a tens-of-megabytes
/// stall. Past it we refuse with [`RouteError::TooFar`] rather than silently
/// grinding — lifting the ceiling is not the fix, routing the long haul over the
/// coarser zooms is (see ADR-037).
pub const MAX_ROUTE_TILES: usize = 96;

/// Roughly how far apart two points can be, straight-line, before the corridor
/// exceeds [`MAX_ROUTE_TILES`] — the reach of street-level routing. Derived from
/// that budget and the corridor shape, and used only to phrase the "too far"
/// message; the tile count is the real gate.
pub const MAX_ROUTE_KM: f64 = 22.0;

/// The zoom the long haul is planned on when the street-level corridor will not
/// fit ([`plan_corridor`]).
///
/// **Measured 2026-08-08, Groningen → Amsterdam (180 km), live tiles.** z14 needs
/// ~1,340 tiles (~648 MB) and is refused; z12 needs 229 and is refused; z10 needs
/// **35 tiles / 2.7 MB** and returns 180 km in 1 h 55 at 94 km/h — within a few
/// per cent of the real drive.
///
/// z12 is not a useful middle rung and the probe is why. At the same tile budget
/// a coarse corridor is *wider in ground terms*, so z10 both costs less and has
/// more room to find the motorway: Groningen → Assen came out 30 km at z10 and
/// 34 km at z12. Two tiers, not four.
pub const LONG_HAUL_ZOOM: u32 = 10;

/// Radius, in [`ROUTING_ZOOM`] tiles, of the detailed patch kept around each
/// endpoint of a staged route.
///
/// **The patches are not a refinement, they are what makes a long route usable.**
/// The coarse network is generalised, so its nearest road to an address is a long
/// way off — measured at **281 m** for the Groningen end of every staged probe.
/// Driver mode calls anything past 60 m off-route, so a coarse-only long route
/// puts the driver off-route on the first frame and straight into a reroute it can
/// never satisfy. ~2 tiles is about 3 km, comfortably past that snap and past
/// [`HANDOVER_M`].
const FINE_PATCH_TILES: f64 = 1.6;

/// How far from each endpoint a staged route hands over from the detailed graph
/// to the coarse one.
///
/// Far enough out that the coarse network is a real road by then (past its own
/// ~281 m snap), close enough in that the fine patch still covers it.
const HANDOVER_M: f64 = 900.0;

/// Half-width of the tile corridor, in tiles: a constant term plus a term that
/// grows with the **square root** of the direct distance, clamped.
///
/// The corridor is a **capsule** around the straight line. Its width must grow
/// *sub-linearly*, or a long route is both long and wide and the tile count blows
/// up quadratically (an early linear term made a cross-province query ask for ~700
/// tiles). `√direct` keeps a long capsule thin — the detour room a route needs does
/// not scale with its length, only with how far a sensible diversion strays from
/// the straight line, which grows far slower. A short city route costs ~12 tiles, a
/// 20 km one ~90.
const CORRIDOR_BASE: f64 = 1.25;
const CORRIDOR_SPREAD: f64 = 0.30;
const CORRIDOR_MAX_HALF_WIDTH: f64 = 5.0;

/// Minimum ring of tiles kept around each endpoint, so a short route (where the
/// corridor is barely wider than one tile) still has streets around it to snap onto.
const ENDPOINT_RING: f64 = 1.0;

/// How far either side of a junction the turn angle is measured over, in metres.
/// Junctions are frequently modelled with short stub segments whose own direction
/// says nothing about the manoeuvre; averaging over a street's worth of geometry
/// does.
const TURN_WINDOW_M: f64 = 25.0;

/// Steps shorter than this are folded into the one before them — see
/// [`merge_short_steps`].
const MIN_STEP_M: f64 = 25.0;

/// Seconds lost passing through a junction on a road that has to give way, for a
/// car. Scaled down by the road's own priority and by the mode — see
/// [`junction_delay`].
///
/// **Without this the cost model cannot tell a through-road from a grid.** Time
/// was purely `length / class speed`, so a kilometre of residential street cost
/// the same whether it crossed two side roads or twenty, and the only thing
/// keeping a route off the back streets was the class speed itself. Every
/// serious router charges for the junction, because that is where the time
/// actually goes in a city.
const JUNCTION_S: f64 = 6.0;

/// A graph node: a vertex on the global z14 lattice (x east, y south). Integers,
/// so equality *is* topological identity — see the module docs.
type Node = (i32, i32);

// ─── Travel modes ────────────────────────────────────────────────────────────

/// How the route is travelled. Organic Maps' three land modes; each one is a
/// different access filter and speed table over the same graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum TravelMode {
    #[default]
    Car,
    Bike,
    Foot,
}

impl TravelMode {
    pub const ALL: [Self; 3] = [Self::Car, Self::Bike, Self::Foot];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Car => "Car",
            Self::Bike => "Bike",
            Self::Foot => "Walk",
        }
    }

    /// Fastest speed the mode can reach anywhere, in m/s. The A* heuristic divides
    /// the straight-line distance by this; anything *higher* than the real maximum
    /// keeps the heuristic admissible (never over-estimates), so this errs upward.
    fn max_speed_ms(self) -> f64 {
        match self {
            Self::Car => 130.0 / 3.6,
            Self::Bike => 25.0 / 3.6,
            Self::Foot => 6.0 / 3.6,
        }
    }

    /// Whether a one-way restriction applies. Cyclists and pedestrians routinely
    /// (and legally, on the contraflow lanes and footways that dominate Dutch
    /// city centres) travel against motor-traffic one-ways, and the tile carries no
    /// `oneway:bicycle` to tell the exceptions apart — so honouring it for them
    /// invents detours that no rider would make.
    fn respects_oneway(self) -> bool {
        matches!(self, Self::Car)
    }
}

// ─── Road classification ─────────────────────────────────────────────────────

/// Shortbread's `streets.kind`, as far as routing cares. `*_link` ramps fold into
/// their parent class and are flagged separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoadKind {
    Motorway,
    Trunk,
    Primary,
    Secondary,
    Tertiary,
    Unclassified,
    Residential,
    LivingStreet,
    Service,
    Pedestrian,
    Footway,
    Steps,
    Path,
    Cycleway,
    Track,
    Busway,
    Rail,
    /// A ferry crossing, from Shortbread's own `ferries` layer rather than from
    /// `streets`. Foot and bike only — see [`Self::speed_kmh`].
    Ferry,
    Other,
}

impl RoadKind {
    fn parse(kind: &str) -> Self {
        // Ramps carry their parent's class ("motorway_link"); the tile also has a
        // `link` boolean, but folding the suffix here keeps this total.
        let base = kind.strip_suffix("_link").unwrap_or(kind);
        match base {
            "motorway" => Self::Motorway,
            "trunk" => Self::Trunk,
            "primary" => Self::Primary,
            "secondary" => Self::Secondary,
            "tertiary" => Self::Tertiary,
            "unclassified" => Self::Unclassified,
            "residential" => Self::Residential,
            "living_street" => Self::LivingStreet,
            "service" => Self::Service,
            "pedestrian" => Self::Pedestrian,
            "footway" => Self::Footway,
            "steps" => Self::Steps,
            "path" | "bridleway" => Self::Path,
            "cycleway" => Self::Cycleway,
            "track" => Self::Track,
            "busway" => Self::Busway,
            "rail" | "light_rail" | "subway" | "tram" | "monorail" | "funicular" => Self::Rail,
            _ => Self::Other,
        }
    }

    /// Travel speed in km/h, or `None` when the mode may not use this road at all.
    ///
    /// These are *routing* speeds, not legal limits: a residential street signposted
    /// at 30 is driven at rather less once junctions and parked cars are counted, and
    /// the numbers below are chosen so the relative ranking (which is what picks the
    /// route) matches how the network is actually driven.
    #[must_use]
    pub fn speed_kmh(self, mode: TravelMode) -> Option<f64> {
        match mode {
            TravelMode::Car => Some(match self {
                Self::Motorway => 100.0,
                Self::Trunk => 80.0,
                Self::Primary => 55.0,
                Self::Secondary => 45.0,
                Self::Tertiary => 40.0,
                Self::Unclassified => 35.0,
                Self::Residential => 25.0,
                Self::LivingStreet => 12.0,
                Self::Service => 12.0,
                Self::Track => 15.0,
                // No cars on foot/cycle ways, in bus lanes, or on rails — and no
                // cars on a ferry, which is the conservative half of a genuine
                // ambiguity. Shortbread's `ferries` layer carries `kind = ferry`
                // and a name, and nothing that separates a car ferry from a
                // passenger pontje. Every car ferry takes foot passengers and no
                // foot ferry takes cars, so foot and bike are safe on any of them
                // and a car is safe on none: routing a driver to a crossing their
                // vehicle cannot board is a failure they discover at the water.
                Self::Pedestrian
                | Self::Footway
                | Self::Steps
                | Self::Path
                | Self::Cycleway
                | Self::Busway
                | Self::Rail
                | Self::Ferry
                | Self::Other => return None,
            }),
            TravelMode::Bike => Some(match self {
                // Motorways and trunk roads are closed to bikes.
                Self::Motorway | Self::Trunk | Self::Rail | Self::Busway => return None,
                // An effective speed, not a sailing speed: what dominates a
                // crossing is the wait, and there is no timetable in a vector
                // tile. 8 km/h puts a 400 m pontje at 3 minutes and a 4 km
                // estuary at half an hour, which is the right order for both.
                // A fixed boarding penalty would be the better model and cannot
                // be expressed here — a ferry line arrives as many segments, and
                // the penalty would be charged once per segment.
                Self::Ferry => 8.0,
                Self::Cycleway => 18.0,
                Self::Primary | Self::Secondary => 16.0,
                Self::Tertiary | Self::Unclassified | Self::Residential => 15.0,
                Self::LivingStreet | Self::Service => 11.0,
                Self::Path | Self::Track => 10.0,
                // Rideable in principle, walked in practice.
                Self::Pedestrian | Self::Footway => 7.0,
                Self::Steps => 2.0,
                Self::Other => return None,
            }),
            TravelMode::Foot => Some(match self {
                Self::Motorway | Self::Trunk | Self::Rail | Self::Busway => return None,
                Self::Ferry => 8.0,
                Self::Steps => 1.8,
                // Everything else is walked at the same pace; the class only
                // decides *whether* you may, not how fast you go.
                Self::Pedestrian
                | Self::Footway
                | Self::Path
                | Self::Cycleway
                | Self::Track
                | Self::LivingStreet
                | Self::Service
                | Self::Residential
                | Self::Unclassified
                | Self::Tertiary
                | Self::Secondary
                | Self::Primary => 4.8,
                Self::Other => return None,
            }),
        }
    }

    /// How much of a junction's delay this class actually pays, 0 = none.
    ///
    /// Priority is the whole model: a car on a primary road crosses a side street
    /// without slowing, and the same car on that side street waits to get out.
    /// Motorways and trunk roads are graded-separated — their "junctions" are
    /// slip roads, which the geometry already charges for in distance — so they
    /// pay nothing, which is also what keeps a long route on them.
    fn junction_share(self) -> f64 {
        match self {
            // A ferry's "junctions" are its two slipways, and the wait is already
            // in its speed; charging a give-way on top would double-count it.
            Self::Motorway | Self::Trunk | Self::Ferry => 0.0,
            Self::Primary => 0.25,
            Self::Secondary => 0.4,
            Self::Tertiary => 0.6,
            Self::Unclassified | Self::Busway => 0.8,
            _ => 1.0,
        }
    }

    /// Display name for a step whose street has no name in the tile.
    #[must_use]
    pub fn generic_name(self) -> &'static str {
        match self {
            Self::Motorway => "the motorway",
            Self::Trunk => "the trunk road",
            Self::Primary | Self::Secondary | Self::Tertiary => "the main road",
            Self::Cycleway => "the cycle path",
            Self::Footway | Self::Path => "the path",
            Self::Steps => "the steps",
            Self::Pedestrian => "the pedestrian street",
            Self::Ferry => "the ferry",
            Self::Service => "the service road",
            Self::Track => "the track",
            _ => "the road",
        }
    }
}

// ─── Graph ───────────────────────────────────────────────────────────────────

/// One directed traversal of a street segment.
#[derive(Debug, Clone, Copy)]
struct Edge {
    to: u32,
    /// Ground length in metres (see [`RoadGraph::m_per_unit`] on the projection).
    len_m: f32,
    kind: RoadKind,
    /// This direction runs against a one-way restriction — closed to modes that
    /// respect it ([`TravelMode::respects_oneway`]).
    against_oneway: bool,
    /// `bicycle=no` on the source way.
    bike_denied: bool,
    /// Loose surface: costs bikes and walkers speed, cars nothing (they are not
    /// routed onto tracks in any meaningful volume anyway).
    unpaved: bool,
    /// The grade this edge runs at — see [`level_of`].
    ///
    /// Nothing in the *search* reads it: a route's cost and its turns do not depend
    /// on how high the road stands. It is carried because the thing that draws the
    /// route does need it, and cannot work it out. A bridge and the street beneath
    /// it have the same ground position, so no test on the drawn line's coordinates
    /// can say which of them it is on — only the edge it came from can.
    level: i8,
}

/// A street segment carrying a name, used to label the turn list. Kept apart from
/// the routing graph because the names live in a different layer (`street_labels`)
/// with its own, differently-merged geometry — there is no id to join on, so the
/// join is spatial and happens once, on the finished path.
#[derive(Debug, Clone)]
struct LabelSeg {
    a: (f64, f64),
    b: (f64, f64),
    name: u32,
}

/// Bucket size of the spatial index, in lattice units (~90 m at z14).
const BUCKET: f64 = 256.0;

/// How close a node must be to a segment to count as lying *on* it, in lattice
/// units (~0.55 m). Chosen from the data rather than by feel: on the Groningen
/// fixture, widening the tolerance from 0.5 to 1.5 units picks up another 273
/// on-segment nodes, and going on to 3.0 finds just 28 more — so 1.5 sits on the
/// flat part of the curve, past the real coincidences and short of inventing
/// junctions between roads that merely pass close to one another.
const NODE_TOL: f64 = 1.5;

/// The levels a node's own segments occupy fit in a `u16` bitmask, so grades are
/// clamped into a window centred on the ground. Nothing real reaches these: OSM's
/// `layer` is conventionally −5..5 and the deepest stack in the wild is a handful
/// of decks.
const MIN_LEVEL: i8 = -7;
const MAX_LEVEL: i8 = 8;

/// A grade as its bit in the [`RoadGraph::node_levels`] mask.
fn level_bit(level: i8) -> u16 {
    1u16 << (level.clamp(MIN_LEVEL, MAX_LEVEL) - MIN_LEVEL) as u32
}

/// A street's grade: 0 is the ground, a bridge stands above it, a tunnel runs below.
///
/// OSM's `layer` is the authority where the tile carries one — it is the tag that
/// actually orders a stack of decks, and it is what the raster's draw order already
/// sorts on. Where it is absent the flags still say the thing that matters: a
/// mapper who tagged a viaduct without a `layer` did say it is not on the ground.
/// Only the *relative* value is ever used, so mapping every bridge to a single
/// level 1 is enough to separate it from what it crosses.
fn level_of(props: Option<&HashMap<String, Value>>) -> i8 {
    match int_prop(props, "layer") {
        Some(explicit) if explicit != 0 => {
            explicit.clamp(i64::from(MIN_LEVEL), i64::from(MAX_LEVEL)) as i8
        }
        _ if bool_prop(props, "bridge") => 1,
        _ if bool_prop(props, "tunnel") => -1,
        _ => 0,
    }
}

/// A street segment as read from a tile, before noding. Edges are only emitted in
/// [`RoadGraph::finish`], because a segment's endpoints are not known until every
/// tile has contributed the nodes that might split it.
#[derive(Debug, Clone, Copy)]
struct RawSeg {
    a: u32,
    b: u32,
    kind: RoadKind,
    /// Travelling a→b is barred by a one-way.
    fwd_blocked: bool,
    /// Travelling b→a is barred by a one-way.
    rev_blocked: bool,
    bike_denied: bool,
    unpaved: bool,
    /// The grade this segment runs at — see [`level_of`]. Read by the noding pass,
    /// which will not invent a junction between two levels, and carried onto
    /// [`Edge`] and out through [`Route::levels`] for the thing that *draws* the
    /// route. Nothing in the search reads it: a route's cost and its turns do not
    /// depend on how high the road stands.
    level: i8,
}

/// The routable street network, stitched from any number of MVT tiles.
pub struct RoadGraph {
    index: HashMap<Node, u32>,
    /// Node coordinates on the global lattice, indexed by node id.
    coords: Vec<(f64, f64)>,
    /// Segments as read from the tiles; consumed by [`Self::finish`].
    raw: Vec<RawSeg>,
    /// Adjacency, empty until [`Self::finish`] runs.
    edges: Vec<Vec<Edge>>,
    /// Whether the noding pass has run — routing before it would silently see a
    /// graph with no edges at all.
    built: bool,
    /// Bucket → node ids, for snapping a lat/lng onto the network.
    node_buckets: HashMap<(i32, i32), Vec<u32>>,
    /// The grades meeting at each node, one bit per level, indexed by node id.
    ///
    /// Written from the *endpoints* of every raw segment, so it answers "is there a
    /// street at this grade that genuinely reaches this point" — which is the
    /// question [`Self::finish`] has to settle before inventing a junction there. A
    /// bridge's abutment is shared with the ground road, so it carries both bits and
    /// stays a junction; a vertex under a viaduct carries only its own.
    node_levels: Vec<u16>,
    /// Whether each node is the *end* of a line some tile drew, rather than a
    /// vertex partway along one. The other half of what [`Self::finish`] needs
    /// before inventing a junction — see the module docs.
    node_terminal: Vec<bool>,
    /// How many junctions [`Self::finish`]'s two guards turned down, split by which
    /// one refused it. See there.
    mid_span_refused: usize,
    cross_level_refused: usize,
    names: Vec<String>,
    label_segs: Vec<LabelSeg>,
    /// Bucket → label segment ids.
    label_buckets: HashMap<(i32, i32), Vec<u32>>,
    /// Ground metres per lattice unit at the graph's reference latitude.
    ///
    /// Web Mercator is conformal, so one scale serves both axes — but it varies
    /// with latitude, and this is fixed at the corridor's midpoint. Over a 20 km
    /// corridor at 53°N that is a ~0.2% error on reported distances, which is well
    /// inside the error of the speed table it feeds.
    m_per_unit: f64,
    /// Reference latitude, kept so lattice → lat/lng can be inverted.
    zoom_n: f64,
    /// The tile zoom this graph's lattice belongs to. Every coordinate in it —
    /// node keys, bucket ids, `m_per_unit` — is relative to this, so tiles of any
    /// other zoom cannot be added to it. See [`Self::new_at`].
    zoom: u32,
}

impl RoadGraph {
    /// An empty street-level graph scaled for routes around `center_lat`.
    #[must_use]
    pub fn new(center_lat: f64) -> Self {
        Self::new_at(ROUTING_ZOOM, center_lat)
    }

    /// An empty graph on the lattice of an arbitrary tile `zoom`.
    ///
    /// [`ROUTING_ZOOM`] is the only zoom with the complete network and is what
    /// `new` gives you. The coarser zooms carry a *generalised* road hierarchy —
    /// measured 2026-08-08 against the VersaTiles Shortbread build: z8 has
    /// motorway/trunk/primary, z10 adds secondary, tertiary and residential, z12
    /// adds unclassified and service, and only z13/z14 carry the walking and
    /// cycling network. That is what makes long-haul routing affordable: the
    /// corridor for a 200 km route is ~1,340 tiles at z14 and ~50 at z10.
    ///
    /// Two things do not survive generalisation and callers must know it.
    /// **`oneway` is a z14-only property** (checked on Groningen city-centre
    /// ancestor tiles, which certainly have one-ways), so a coarse graph is
    /// effectively undirected — acceptable on the long haul, where a dual
    /// carriageway is two separate ways anyway, and not acceptable in a city.
    /// And the geometry is simplified, so distances off a coarse graph run a
    /// little short and its turn list is crude.
    #[must_use]
    pub fn new_at(zoom: u32, center_lat: f64) -> Self {
        Self {
            index: HashMap::new(),
            coords: Vec::new(),
            raw: Vec::new(),
            edges: Vec::new(),
            built: false,
            node_buckets: HashMap::new(),
            node_levels: Vec::new(),
            node_terminal: Vec::new(),
            mid_span_refused: 0,
            cross_level_refused: 0,
            names: Vec::new(),
            label_segs: Vec::new(),
            label_buckets: HashMap::new(),
            m_per_unit: tile_to_meters(zoom, center_lat) / EXTENT,
            zoom_n: 2.0_f64.powi(zoom as i32),
            zoom,
        }
    }

    /// The tile zoom this graph was built at.
    #[must_use]
    pub fn zoom(&self) -> u32 {
        self.zoom
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.coords.len()
    }

    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.iter().map(Vec::len).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.coords.is_empty()
    }

    /// Fold one tile's `streets` (and `street_labels`) into the graph.
    ///
    /// Re-adding a tile is harmless in the sense that node identity is stable, but
    /// it would duplicate every edge — the caller tracks which tiles are in.
    pub fn add_tile(&mut self, tile_data: &TileData, tile: (i64, i64)) -> Result<(), String> {
        for meta in tile_data.layers() {
            // `ferries` is its own Shortbread layer, present from z10 up wherever
            // there is a crossing (verified 2026-08-08 on the Westerschelde
            // tiles — its absence from the Groningen fixture is a fact about
            // Groningen). It is both: routable geometry *and* the only place its
            // own name lives, since `street_labels` does not cover it.
            let (routable, labelled) = match meta.name.as_str() {
                "streets" => (true, false),
                "street_labels" => (false, true),
                "ferries" => (true, true),
                _ => continue,
            };
            let features = tile_data.features(meta);
            // Normalise a source that ships a different extent onto our lattice,
            // so tiles from mixed sources still stitch (see the module docs).
            let extent = f64::from(TileData::extent(meta));
            let scale = EXTENT / extent;
            let ox = tile.0 as f64 * EXTENT;
            let oy = tile.1 as f64 * EXTENT;
            // A tile's geometry belongs to that tile, plus the buffer an encoder
            // clips with. Anything further out is not a street this tile can
            // speak for, and — the reason this is a hard gate rather than a
            // tidiness rule — `mvt-reader` clips its coordinate cursor only at
            // `i32::MAX`, never to the extent, so a crafted or corrupt body puts
            // a vertex at ~2.1e9. One of those in the graph turns the noding
            // pass's bucket sweep into ~10^14 iterations and the label index into
            // that many allocations. `None` here drops the segment it belongs to.
            let margin = extent * f64::from(TILE_BUFFER_FRAC);
            let to_global = |c: &geo_types::Coord<f32>| -> Option<(f64, f64)> {
                let (x, y) = (f64::from(c.x), f64::from(c.y));
                // Positive test, so a NaN — false against every comparison — is
                // rejected rather than admitted.
                if !(x >= -margin && x <= extent + margin && y >= -margin && y <= extent + margin) {
                    return None;
                }
                Some((ox + x * scale, oy + y * scale))
            };

            let ferry = meta.name == "ferries";
            for feature in features {
                if routable {
                    self.add_street(feature, &to_global, ferry);
                }
                if labelled {
                    self.add_labels(feature, &to_global);
                }
            }
        }
        Ok(())
    }

    /// Node the collected segments and emit the adjacency. Must be called once,
    /// after the last [`Self::add_tile`] and before any [`Self::route`].
    ///
    /// The pass splits every segment at the nodes that lie on it (within
    /// [`NODE_TOL`]) but are not its endpoints — the tees that per-feature
    /// generalisation left unconnected. See the module docs for why a graph built
    /// without this looks fine and routes badly.
    pub fn finish(&mut self) {
        if self.built {
            return;
        }
        self.built = true;
        self.edges = vec![Vec::new(); self.coords.len()];

        let raw = std::mem::take(&mut self.raw);
        let mut splits: Vec<(f64, u32)> = Vec::new();
        // Candidates the grade guard turned down. Counted rather than merely
        // skipped: a number that is zero over a city means the levels never reached
        // the graph — a source that ships no `bridge`/`layer` at all reads exactly
        // like one whose bridges cross nothing, and only this tells them apart.
        let mut cross_level = 0usize;
        let mut mid_span = 0usize;
        for seg in &raw {
            let (a, b) = (self.coords[seg.a as usize], self.coords[seg.b as usize]);
            splits.clear();

            // Candidate splitters: every node in the buckets the segment's bounding
            // box touches. Segments are short (a city block at most), so this is a
            // handful of buckets each.
            let (x0, x1) = minmax(a.0, b.0);
            let (y0, y1) = minmax(a.1, b.1);
            let (Some((bx0, bx1)), Some((by0, by1))) = (
                bucket_span(x0 - NODE_TOL, x1 + NODE_TOL),
                bucket_span(y0 - NODE_TOL, y1 + NODE_TOL),
            ) else {
                // Wider than a segment of one tile's geometry can be, so it is not
                // a street and the sweep over its bbox would not terminate in any
                // useful time. Emit it as it stands — unnoded, connected to
                // exactly what it was built from — rather than dropping an edge
                // the caller already has nodes for.
                self.emit(seg, seg.a, seg.b);
                continue;
            };
            for bx in bx0..=bx1 {
                for by in by0..=by1 {
                    let Some(ids) = self.node_buckets.get(&(bx, by)) else {
                        continue;
                    };
                    for &id in ids {
                        if id == seg.a || id == seg.b {
                            continue;
                        }
                        let p = self.coords[id as usize];
                        let (d, t) = point_seg_projection(p, a, b);
                        // Strictly interior: an endpoint-adjacent hit is already
                        // connected, and admitting t = 0/1 would emit zero-length
                        // edges that the turn list then has to filter out.
                        if d > NODE_TOL || t <= 1e-6 || t >= 1.0 - 1e-6 {
                            continue;
                        }
                        // Both guards are tested *after* the geometry, so their
                        // counters mean "junctions this refused" and not "bucket
                        // neighbours it looked at" — most of a bucket is nowhere near
                        // the segment.
                        //
                        // Only a *dangle* may split. The tee this pass exists to
                        // repair is a line that stops dead on another line, its
                        // shared vertex generalised away; a line that carries on past
                        // the contact was never touching it. That is the difference
                        // between a junction and a crossing, and it is the one that
                        // survives having no way ids to ask.
                        if !self.node_terminal[id as usize] {
                            mid_span += 1;
                            continue;
                        }
                        // …and only at its own grade. A slip road really does end on
                        // the carriageway it joins, so being a dangle is not enough:
                        // a viaduct's abutment is a dangle sitting directly over the
                        // street it lands beside.
                        if self.node_levels[id as usize] & level_bit(seg.level) == 0 {
                            cross_level += 1;
                            continue;
                        }
                        splits.push((t, id));
                    }
                }
            }

            if splits.is_empty() {
                self.emit(seg, seg.a, seg.b);
                continue;
            }
            splits.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap_or(std::cmp::Ordering::Equal));
            splits.dedup_by_key(|(_, id)| *id);
            let mut prev = seg.a;
            for &(_, id) in &splits {
                self.emit(seg, prev, id);
                prev = id;
            }
            self.emit(seg, prev, seg.b);
        }
        self.cross_level_refused = cross_level;
        self.mid_span_refused = mid_span;
        tracing::debug!(
            nodes = self.coords.len(),
            segments = raw.len(),
            mid_span,
            cross_level,
            "noded the street graph"
        );
    }

    /// Emit both traversals of one (possibly split) segment.
    fn emit(&mut self, seg: &RawSeg, a: u32, b: u32) {
        if a == b {
            return;
        }
        let (pa, pb) = (self.coords[a as usize], self.coords[b as usize]);
        let len_m =
            (((pb.0 - pa.0).powi(2) + (pb.1 - pa.1).powi(2)).sqrt() * self.m_per_unit) as f32;
        self.edges[a as usize].push(Edge {
            to: b,
            len_m,
            kind: seg.kind,
            against_oneway: seg.fwd_blocked,
            bike_denied: seg.bike_denied,
            unpaved: seg.unpaved,
            level: seg.level,
        });
        self.edges[b as usize].push(Edge {
            to: a,
            len_m,
            kind: seg.kind,
            against_oneway: seg.rev_blocked,
            bike_denied: seg.bike_denied,
            unpaved: seg.unpaved,
            level: seg.level,
        });
    }

    fn add_street(
        &mut self,
        feature: &Feature,
        to_global: &impl Fn(&geo_types::Coord<f32>) -> Option<(f64, f64)>,
        ferry: bool,
    ) {
        let props = feature.properties.as_ref();
        // The `ferries` layer's own `kind` is `ferry`, which `RoadKind::parse`
        // would read as `Other` and drop; the layer it came from is what says
        // what it is.
        let kind = if ferry {
            RoadKind::Ferry
        } else {
            RoadKind::parse(str_prop(props, "kind").unwrap_or(""))
        };
        // Rails share the layer with streets and are routable for nobody.
        if kind == RoadKind::Rail || bool_prop(props, "rail") {
            return;
        }
        let oneway = bool_prop(props, "oneway");
        // The way is digitised against its legal direction — the one-way runs
        // backwards along the geometry.
        let reversed = bool_prop(props, "oneway_reverse");
        let bike_denied = matches!(str_prop(props, "bicycle"), Some("no"));
        let unpaved = matches!(
            str_prop(props, "surface"),
            Some(
                "unpaved"
                    | "gravel"
                    | "ground"
                    | "dirt"
                    | "sand"
                    | "grass"
                    | "compacted"
                    | "cobblestone"
            )
        );

        // `oneway_reverse` says the legal direction runs *against* the digitised
        // geometry, so it swaps which traversal is barred.
        let (fwd_blocked, rev_blocked) = match (oneway, reversed) {
            (false, _) => (false, false),
            (true, false) => (false, true),
            (true, true) => (true, false),
        };
        // A ferry has neither tag and lands on the ground, which is right: it meets
        // the road network at slipways that are ordinary street vertices.
        let level = level_of(props);
        let mut add_line = |line: &geo_types::LineString<f32>| {
            let pts: Vec<Option<(f64, f64)>> = line.coords().map(to_global).collect();
            // The first and last node this line actually contributes a segment to.
            // Taken from the retained run rather than from `pts`, so a vertex the
            // window rejected cannot mark a terminal — and, more importantly, a
            // vertex that joins no segment is never interned at all: one of those
            // is an orphan node in the snap index, and the hostile fixtures are
            // built out of exactly such isolated coordinates.
            let (mut first, mut last) = (None, None);
            for w in pts.windows(2) {
                // One endpoint outside the tile's window is enough to drop the
                // segment; the rest of the line is kept, since a way that leaves
                // the window comes back from the neighbouring tile with the same
                // lattice node and stitches there.
                let (Some(a), Some(b)) = (w[0], w[1]) else {
                    continue;
                };
                let (ia, ib) = (self.node(a, level), self.node(b, level));
                if ia == ib {
                    continue; // zero-length after quantisation
                }
                first = first.or(Some(ia));
                last = Some(ib);
                self.raw.push(RawSeg {
                    a: ia,
                    b: ib,
                    kind,
                    fwd_blocked,
                    rev_blocked,
                    bike_denied,
                    unpaved,
                    level,
                });
            }
            // Shortbread merges ways by attribute, so a run of bridge stops being
            // one line exactly where the `bridge` flag stops — which puts these two
            // marks on the abutments, the places a deck legitimately joins the
            // ground.
            for id in [first, last].into_iter().flatten() {
                self.node_terminal[id as usize] = true;
            }
        };

        match &feature.geometry {
            Geometry::LineString(ls) => add_line(ls),
            Geometry::MultiLineString(mls) => mls.iter().for_each(add_line),
            _ => {}
        }
    }

    fn add_labels(
        &mut self,
        feature: &Feature,
        to_global: &impl Fn(&geo_types::Coord<f32>) -> Option<(f64, f64)>,
    ) {
        let props = feature.properties.as_ref();
        let Some(name) = str_prop(props, "name").filter(|n| !n.is_empty()) else {
            return;
        };
        let name_id = self.names.len() as u32;
        self.names.push(name.to_string());

        let mut any = false;
        let mut add_line = |line: &geo_types::LineString<f32>| {
            let pts: Vec<Option<(f64, f64)>> = line.coords().map(to_global).collect();
            for w in pts.windows(2) {
                let (Some(a), Some(b)) = (w[0], w[1]) else {
                    continue;
                };
                // Register in every bucket the segment's bounding box touches, so a
                // 3×3 bucket probe around a point always sees a nearby segment.
                let (x0, x1) = minmax(a.0, b.0);
                let (y0, y1) = minmax(a.1, b.1);
                let (Some((bx0, bx1)), Some((by0, by1))) =
                    (bucket_span(x0, x1), bucket_span(y0, y1))
                else {
                    // Longer than any street name could label. The window above
                    // should already have refused it; this is the backstop that
                    // makes the index cost bounded by construction rather than by
                    // the window being complete.
                    continue;
                };
                let id = self.label_segs.len() as u32;
                self.label_segs.push(LabelSeg {
                    a,
                    b,
                    name: name_id,
                });
                for bx in bx0..=bx1 {
                    for by in by0..=by1 {
                        self.label_buckets.entry((bx, by)).or_default().push(id);
                    }
                }
                any = true;
            }
        };
        match &feature.geometry {
            Geometry::LineString(ls) => add_line(ls),
            Geometry::MultiLineString(mls) => mls.iter().for_each(add_line),
            _ => {}
        }
        if !any {
            self.names.pop();
        }
    }

    /// Intern a lattice point as a node id, recording that a street at `level`
    /// reaches it.
    ///
    /// Node identity stays the bare coordinate, *not* the coordinate and the level:
    /// a bridge meets the ground through a vertex it shares with the road at its
    /// abutment, so keying the level in would disconnect every bridge from both its
    /// ends. The level is accumulated beside the node instead, for the noding pass
    /// to consult — see the module docs.
    fn node(&mut self, p: (f64, f64), level: i8) -> u32 {
        let key: Node = (p.0.round() as i32, p.1.round() as i32);
        let bit = level_bit(level);
        if let Some(&id) = self.index.get(&key) {
            self.node_levels[id as usize] |= bit;
            return id;
        }
        let id = self.coords.len() as u32;
        self.index.insert(key, id);
        self.coords.push((f64::from(key.0), f64::from(key.1)));
        self.node_levels.push(bit);
        self.node_terminal.push(false);
        self.node_buckets
            .entry((bucket(f64::from(key.0)), bucket(f64::from(key.1))))
            .or_default()
            .push(id);
        id
    }

    // ─── Coordinate conversion ───────────────────────────────────────────────

    /// WGS84 → global lattice units at [`ROUTING_ZOOM`].
    #[must_use]
    pub fn lat_lng_to_lattice(lat: f64, lng: f64) -> (f64, f64) {
        lattice_at(ROUTING_ZOOM, lat, lng)
    }

    /// WGS84 → this graph's own lattice. Everything that indexes into the graph
    /// has to go through here rather than through the [`ROUTING_ZOOM`] helper
    /// above, or a coarse graph silently looks up a point 16 tiles away.
    #[must_use]
    fn lattice(&self, lat: f64, lng: f64) -> (f64, f64) {
        lattice_at(self.zoom, lat, lng)
    }

    /// Inverse of [`Self::lat_lng_to_lattice`].
    #[must_use]
    fn lattice_to_lat_lng(&self, x: f64, y: f64) -> (f64, f64) {
        let (fx, fy) = (x / EXTENT, y / EXTENT);
        let lng = fx / self.zoom_n * 360.0 - 180.0;
        let lat = ((1.0 - 2.0 * fy / self.zoom_n) * std::f64::consts::PI)
            .sinh()
            .atan()
            .to_degrees();
        (lat, lng)
    }

    /// Nearest node with at least one edge the mode may use, within `max_m`.
    ///
    /// The access filter matters: snapping a car onto the footpath that happens to
    /// run closest to the address strands the search at a node it can never leave.
    /// [`Self::route`] snaps into the [routable core](Self::routable_core) instead —
    /// this plain nearest-usable-node is a public primitive kept for callers that
    /// want the raw snap, and is exercised by the routing diagnostics.
    #[must_use]
    #[allow(dead_code)]
    pub fn nearest_node(&self, lat: f64, lng: f64, mode: TravelMode, max_m: f64) -> Option<u32> {
        self.nearest_node_where(lat, lng, max_m, |id| {
            self.edges[id as usize]
                .iter()
                .any(|e| self.edge_cost(e, mode).is_some())
        })
    }

    /// Nearest node satisfying `accept`, within `max_m`. The ring-by-ring probe is
    /// the same whether the predicate is "usable by the mode" ([`Self::nearest_node`])
    /// or "in the routable core" ([`Self::route`]); only the filter differs.
    fn nearest_node_where(
        &self,
        lat: f64,
        lng: f64,
        max_m: f64,
        accept: impl Fn(u32) -> bool,
    ) -> Option<u32> {
        let p = self.lattice(lat, lng);
        let max_units = max_m / self.m_per_unit;
        let reach = (max_units / BUCKET).ceil() as i32;
        let (bx, by) = (bucket(p.0), bucket(p.1));
        // Widen the probe ring by ring; the first ring that yields a candidate is
        // not necessarily the closest, so scan one ring past the first hit.
        let mut best: Option<(f64, u32)> = None;
        for r in 0..=reach.max(1) {
            for dx in -r..=r {
                for dy in -r..=r {
                    // Only the new ring's buckets.
                    if r > 0 && dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let Some(ids) = self.node_buckets.get(&(bx + dx, by + dy)) else {
                        continue;
                    };
                    for &id in ids {
                        if !accept(id) {
                            continue;
                        }
                        let c = self.coords[id as usize];
                        let d2 = (c.0 - p.0).powi(2) + (c.1 - p.1).powi(2);
                        if best.is_none_or(|(bd, _)| d2 < bd) {
                            best = Some((d2, id));
                        }
                    }
                }
            }
            // One extra ring after the first hit covers the case where a nearer
            // node sits just across a bucket boundary.
            if let Some((d2, id)) = best {
                if d2.sqrt() <= f64::from(r) * BUCKET || r >= reach {
                    return (d2.sqrt() <= max_units).then_some(id);
                }
            }
        }
        best.filter(|(d2, _)| d2.sqrt() <= max_units)
            .map(|(_, id)| id)
    }

    /// Membership mask of the graph's **routable core** for a mode: the largest
    /// strongly-connected component of the mode-filtered directed graph.
    ///
    /// This is the fix for a real failure. A car snapped to the geometrically
    /// nearest node landed on a genuinely one-way residential street (`oneway=yes`
    /// in the source, so the parse was right) whose forward-reachable set was a
    /// three-node dead-end pocket; the cross-town route then returned `NoPath`,
    /// while the same pair routed for bikes, which ignore one-ways. In a road
    /// network the drivable through-network is one giant SCC and the stubs, service
    /// loops and one-way pockets are tiny ones, so snapping both endpoints into the
    /// largest SCC guarantees a path exists between them — the standard answer, and
    /// what a stub a few metres off the through-road deserves.
    ///
    /// Kosaraju, iterative (a corridor of tens of thousands of nodes has chains far
    /// deeper than the stack): forward post-order, then peel components off the
    /// reverse graph in reverse post-order.
    fn routable_core(&self, mode: TravelMode) -> Vec<bool> {
        let n = self.coords.len();
        let usable_edge = |e: &Edge| self.edge_cost(e, mode).is_some();

        // Pass 1 — forward DFS, record finish order.
        let mut visited = vec![false; n];
        let mut order = Vec::with_capacity(n);
        // Stack frames carry the node and how far through its edge list we are, so
        // the post-order push happens when a node's edges are exhausted.
        let mut stack: Vec<(u32, usize)> = Vec::new();
        for seed in 0..n as u32 {
            if visited[seed as usize] {
                continue;
            }
            visited[seed as usize] = true;
            stack.push((seed, 0));
            while let Some((node, ei)) = stack.last_mut().copied() {
                let adj = &self.edges[node as usize];
                if ei < adj.len() {
                    stack.last_mut().unwrap().1 += 1;
                    let e = &adj[ei];
                    if usable_edge(e) && !visited[e.to as usize] {
                        visited[e.to as usize] = true;
                        stack.push((e.to, 0));
                    }
                } else {
                    order.push(node);
                    stack.pop();
                }
            }
        }

        // Reverse adjacency over the usable edges only (built once).
        let mut rev: Vec<Vec<u32>> = vec![Vec::new(); n];
        for (from, adj) in self.edges.iter().enumerate() {
            for e in adj {
                if usable_edge(e) {
                    rev[e.to as usize].push(from as u32);
                }
            }
        }

        // Pass 2 — DFS the reverse graph in reverse finish order; each tree is one
        // SCC. Keep the largest.
        let mut comp = vec![u32::MAX; n];
        let mut best_root = u32::MAX;
        let mut best_size = 0usize;
        let mut label = 0u32;
        let mut dfs: Vec<u32> = Vec::new();
        for &seed in order.iter().rev() {
            if comp[seed as usize] != u32::MAX {
                continue;
            }
            dfs.push(seed);
            comp[seed as usize] = label;
            let mut size = 0usize;
            while let Some(node) = dfs.pop() {
                size += 1;
                for &prev in &rev[node as usize] {
                    if comp[prev as usize] == u32::MAX {
                        comp[prev as usize] = label;
                        dfs.push(prev);
                    }
                }
            }
            if size > best_size {
                best_size = size;
                best_root = label;
            }
            label += 1;
        }

        comp.iter().map(|&c| c == best_root).collect()
    }

    /// Which nodes are junctions for `mode`: three or more distinct neighbours it
    /// may actually use.
    ///
    /// Mode-dependent and deliberately so. A residential street crossed by a
    /// footpath is a junction for a walker and a straight run for a driver, and a
    /// mode-agnostic degree would charge the car a give-way at every zebra in the
    /// city — which is the sort of plausible-looking penalty that would quietly
    /// re-route everything and be very hard to trace back.
    ///
    /// One pass over the adjacency; the same shape [`Self::routable_core`] already
    /// costs, and both are computed once per solve rather than per expansion.
    fn junction_mask(&self, mode: TravelMode) -> Vec<bool> {
        (0..self.edges.len() as u32)
            .map(|n| self.is_junction(n, mode))
            .collect()
    }

    /// The fastest any edge *in this graph* can be travelled by `mode`, in m/s.
    ///
    /// The A* heuristic divides by this. [`TravelMode::max_speed_ms`] is the
    /// mode's global ceiling — 130 km/h for a car — and a city corridor holds
    /// nothing above 55, so the heuristic was roughly a factor of two short and
    /// the search degenerated into Dijkstra over the whole corridor. Taking the
    /// maximum over the graph keeps it admissible (no edge can beat an edge-wise
    /// maximum) while making it as tight as the data allows, which is exactly
    /// where the saving is largest: the long corridors.
    fn top_speed_ms(&self, mode: TravelMode) -> f64 {
        let fastest = self
            .edges
            .iter()
            .flatten()
            .filter_map(|e| e.kind.speed_kmh(mode))
            .fold(0.0_f64, f64::max);
        if fastest > 0.0 {
            fastest / 3.6
        } else {
            // No usable edge at all; the search will fail anyway, and a zero here
            // would be a division by zero rather than a `NoPath`.
            mode.max_speed_ms()
        }
    }

    /// Traversal time in seconds, or `None` if the mode may not use this edge.
    fn edge_cost(&self, edge: &Edge, mode: TravelMode) -> Option<f64> {
        if edge.against_oneway && mode.respects_oneway() {
            return None;
        }
        if edge.bike_denied && mode == TravelMode::Bike {
            return None;
        }
        let mut kmh = edge.kind.speed_kmh(mode)?;
        if edge.unpaved && mode != TravelMode::Car {
            kmh *= 0.75;
        }
        Some(f64::from(edge.len_m) / (kmh / 3.6))
    }

    /// The name of the street nearest to a lattice point, if one is close enough
    /// to plausibly be the same street (~25 m).
    fn name_at(&self, p: (f64, f64)) -> Option<&str> {
        let max = 40.0; // lattice units ≈ 25 m
        let (bx, by) = (bucket(p.0), bucket(p.1));
        let mut best: Option<(f64, u32)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                let Some(ids) = self.label_buckets.get(&(bx + dx, by + dy)) else {
                    continue;
                };
                for &id in ids {
                    let seg = &self.label_segs[id as usize];
                    let d = point_seg_distance(p, seg.a, seg.b);
                    if d < max && best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, seg.name));
                    }
                }
            }
        }
        best.map(|(_, n)| self.names[n as usize].as_str())
    }

    /// Every node reachable from `from` under `mode` (the connected component it
    /// sits in). A diagnostic for "why does this route return `NoPath`": if the
    /// start's component does not contain the goal, the network is severed for that
    /// mode, and the severing edge is at the boundary between the two.
    #[cfg(test)]
    pub(crate) fn reachable(&self, from: u32, mode: TravelMode) -> Vec<u32> {
        let mut seen = vec![false; self.coords.len()];
        let mut stack = vec![from];
        seen[from as usize] = true;
        let mut out = Vec::new();
        while let Some(v) = stack.pop() {
            out.push(v);
            for e in &self.edges[v as usize] {
                if self.edge_cost(e, mode).is_some() && !seen[e.to as usize] {
                    seen[e.to as usize] = true;
                    stack.push(e.to);
                }
            }
        }
        out
    }

    /// A node's WGS84 position — for naming where a diagnostic found a break.
    #[cfg(test)]
    pub(crate) fn node_lat_lng(&self, id: u32) -> (f64, f64) {
        let c = self.coords[id as usize];
        self.lattice_to_lat_lng(c.0, c.1)
    }

    /// Size of the biggest connected component for a mode, and how many nodes the
    /// mode can use at all. Only the tests need this, but it belongs next to the
    /// adjacency it walks.
    #[cfg(test)]
    fn largest_component(&self, mode: TravelMode) -> (usize, usize) {
        let usable: Vec<bool> = (0..self.coords.len())
            .map(|i| {
                self.edges[i]
                    .iter()
                    .any(|e| self.edge_cost(e, mode).is_some())
            })
            .collect();
        let mut seen = vec![false; self.coords.len()];
        let mut best = 0;
        for start in 0..self.coords.len() {
            if seen[start] || !usable[start] {
                continue;
            }
            let mut stack = vec![start as u32];
            seen[start] = true;
            let mut size = 0;
            while let Some(v) = stack.pop() {
                size += 1;
                for e in &self.edges[v as usize] {
                    if self.edge_cost(e, mode).is_some() && !seen[e.to as usize] {
                        seen[e.to as usize] = true;
                        stack.push(e.to);
                    }
                }
            }
            best = best.max(size);
        }
        (best, usable.iter().filter(|b| **b).count())
    }

    // ─── Search ──────────────────────────────────────────────────────────────

    /// Find the quickest route between two coordinates for `mode`.
    pub fn route(
        &self,
        from: (f64, f64),
        to: (f64, f64),
        mode: TravelMode,
    ) -> Result<Route, RouteError> {
        // `!built` means `finish` was never called, so there are no edges at all —
        // indistinguishable, from here, from having no tiles.
        if self.is_empty() || !self.built {
            return Err(RouteError::NoData);
        }
        // Generous snap radius: an address is often set back from the carriageway,
        // and a map click lands wherever the user aimed.
        const SNAP_M: f64 = 500.0;
        // Snap into the routable core (largest SCC), not merely onto the nearest
        // usable node — otherwise a one-way stub a few metres from the address traps
        // the search (see `routable_core`). Both ends in one SCC ⇒ a path exists, so
        // `NoPath` after this is a genuinely disconnected corridor, not a bad snap.
        let core = self.routable_core(mode);
        let in_core = |id: u32| core[id as usize];
        // Failing to snap into the core is two different failures wearing one
        // face. If there is no usable road near the point at all, "no road near
        // the destination" is the truth. If there is one and it merely isn't in
        // the same component — a town across an estuary that only a ferry
        // reaches, and this mode may not use the ferry — then saying there is no
        // road there is a plain lie about a place with streets in it, and the
        // honest answer is that nothing connects the two for this mode.
        let snap = |at: (f64, f64), absent: RouteError| {
            self.nearest_node_where(at.0, at.1, SNAP_M, in_core)
                .ok_or_else(|| {
                    if self.nearest_node(at.0, at.1, mode, SNAP_M).is_some() {
                        RouteError::NoPath
                    } else {
                        absent
                    }
                })
        };
        let start = snap(from, RouteError::NoStart)?;
        let goal = snap(to, RouteError::NoEnd)?;
        if start == goal {
            return Err(RouteError::TooClose);
        }

        let path = self.astar(start, goal, mode).ok_or(RouteError::NoPath)?;
        Ok(self.build_route(&path, mode, from, to))
    }

    /// A* over travel time. The heuristic is the straight-line distance divided by
    /// the fastest speed present in the graph — admissible, since no edge can beat
    /// an edge-wise maximum, and far tighter than the mode's global ceiling
    /// ([`Self::top_speed_ms`]).
    ///
    /// The cost of an expansion is the edge plus the delay of the junction it
    /// arrives at ([`junction_delay`]). Adding a non-negative term to every edge
    /// leaves the heuristic admissible — the real cost only goes up.
    fn astar(&self, start: u32, goal: u32, mode: TravelMode) -> Option<Vec<u32>> {
        let goal_c = self.coords[goal as usize];
        let top = self.top_speed_ms(mode);
        let junction = self.junction_mask(mode);
        let h = |id: u32| -> f64 {
            let c = self.coords[id as usize];
            ((c.0 - goal_c.0).powi(2) + (c.1 - goal_c.1).powi(2)).sqrt() * self.m_per_unit / top
        };

        let mut g: HashMap<u32, f64> = HashMap::new();
        let mut came: HashMap<u32, u32> = HashMap::new();
        let mut open = BinaryHeap::new();
        g.insert(start, 0.0);
        open.push(Candidate {
            priority: Ordered(h(start)),
            node: start,
        });

        while let Some(Candidate { node, priority }) = open.pop() {
            if node == goal {
                let mut path = vec![goal];
                let mut cur = goal;
                while let Some(&prev) = came.get(&cur) {
                    path.push(prev);
                    cur = prev;
                }
                path.reverse();
                return Some(path);
            }
            let here = g.get(&node).copied().unwrap_or(f64::INFINITY);
            // A stale heap entry for a node we have since improved on.
            if priority.0 > here + h(node) + 1e-6 {
                continue;
            }
            for edge in &self.edges[node as usize] {
                let Some(cost) = self.edge_cost(edge, mode) else {
                    continue;
                };
                // Charged on *arrival*, and on the class of the road you arrive
                // on — that is where the priority lives. The goal node collects
                // one too; it is the same constant on every path into it, so it
                // cannot change which path wins.
                let cost = cost
                    + if junction[edge.to as usize] {
                        junction_delay(edge.kind, mode)
                    } else {
                        0.0
                    };
                let tentative = here + cost;
                if tentative + 1e-9 < g.get(&edge.to).copied().unwrap_or(f64::INFINITY) {
                    g.insert(edge.to, tentative);
                    came.insert(edge.to, node);
                    open.push(Candidate {
                        priority: Ordered(tentative + h(edge.to)),
                        node: edge.to,
                    });
                }
            }
        }
        None
    }

    /// Turn a node path into a [`Route`]: the polyline, the totals, and the turn
    /// list.
    ///
    /// Most of the work is *not* emitting instructions. The graph has a node per
    /// vertex, so a gently curving street is a run of small direction changes and a
    /// naive per-vertex classifier narrates it as "bear right, bear left, bear
    /// right…" for 300 m. Two rules fix that, and both are needed:
    ///
    /// * A step can only begin where the network actually branches
    ///   ([`Self::is_junction`]) or where the street name changes. Curvature between
    ///   junctions is not a decision the traveller makes.
    /// * The turn angle is measured over a [`TURN_WINDOW_M`] window either side of
    ///   the junction rather than between the two touching segments, so the stub
    ///   segments junctions are often modelled with cannot fake a turn.
    ///
    /// A final pass folds away steps too short to be worth saying out loud.
    fn build_route(
        &self,
        path: &[u32],
        mode: TravelMode,
        from: (f64, f64),
        to: (f64, f64),
    ) -> Route {
        let mut points = Vec::with_capacity(path.len() + 2);
        // Lead in from and out to the coordinates the user actually asked for, so
        // the drawn line meets the pins instead of stopping at the kerb.
        points.push([from.0, from.1]);
        for &n in path {
            let c = self.coords[n as usize];
            let (lat, lng) = self.lattice_to_lat_lng(c.0, c.1);
            points.push([lat, lng]);
        }
        points.push([to.0, to.1]);

        // A point's grade is the **lowest** of the path edges meeting there, so an
        // abutment — where a bridge run's edge meets a ground edge — counts as
        // ground. That puts the step at the first vertex genuinely on the structure
        // rather than at the one where it begins, which is where the deck's own
        // fascia puts it too. The two pins inherit the end they lead to.
        let mut levels = vec![i8::MAX; points.len()];
        for (i, w) in path.windows(2).enumerate() {
            let level = self.edges[w[0] as usize]
                .iter()
                .filter(|e| e.to == w[1])
                .filter(|e| self.edge_cost(e, mode).is_some())
                .map(|e| e.level)
                .min()
                .unwrap_or(0);
            // `points` is [pin, path…, pin], so path vertex `i` is `points[i + 1]`.
            levels[i + 1] = levels[i + 1].min(level);
            levels[i + 2] = levels[i + 2].min(level);
        }
        // A one-node path has no edge to speak for it, and the pins copy their
        // neighbour rather than inventing a grade of their own.
        for l in &mut levels {
            if *l == i8::MAX {
                *l = 0;
            }
        }
        if points.len() >= 3 {
            levels[0] = levels[1];
            let last = levels.len() - 1;
            levels[last] = levels[last - 1];
        }

        // One hop per traversed edge, with everything the turn list needs.
        let mut hops: Vec<Hop> = Vec::with_capacity(path.len());
        for w in path.windows(2) {
            let (a, b) = (self.coords[w[0] as usize], self.coords[w[1] as usize]);
            let Some(edge) = self.edges[w[0] as usize]
                .iter()
                .filter(|e| e.to == w[1])
                .find(|e| self.edge_cost(e, mode).is_some())
            else {
                continue;
            };
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let norm = dx.hypot(dy);
            if norm <= f64::EPSILON {
                continue;
            }
            hops.push(Hop {
                at: w[0],
                len_m: f64::from(edge.len_m),
                // The junction delay the search paid for this hop is part of the
                // hop's time, not an accounting detail: leave it out and the ETA
                // under-reports every city route, and — worse — reports a *time*
                // for a route that was chosen to minimise a different number.
                duration_s: self.edge_cost(edge, mode).unwrap_or(0.0)
                    + if self.is_junction(w[1], mode) {
                        junction_delay(edge.kind, mode)
                    } else {
                        0.0
                    },
                name: self
                    .name_at(((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0))
                    .map(str::to_string)
                    .unwrap_or_else(|| edge.kind.generic_name().to_string()),
                dir: (dx / norm, dy / norm),
                coord: {
                    let (lat, lng) = self.lattice_to_lat_lng(a.0, a.1);
                    [lat, lng]
                },
            });
        }

        let distance_m = hops.iter().map(|h| h.len_m).sum();
        let duration_s = hops.iter().map(|h| h.duration_s).sum();
        if hops.is_empty() {
            return Route {
                mode,
                points,
                levels,
                distance_m,
                duration_s,
                steps: vec![RouteStep {
                    maneuver: Maneuver::Arrive,
                    name: None,
                    distance_m: 0.0,
                    duration_s: 0.0,
                    at: [to.0, to.1],
                }],
            };
        }

        let mut steps = vec![RouteStep {
            maneuver: Maneuver::Depart,
            name: Some(hops[0].name.clone()),
            distance_m: 0.0,
            duration_s: 0.0,
            at: points[0],
        }];
        for i in 0..hops.len() {
            if i > 0 {
                let name_changed = steps
                    .last()
                    .and_then(|s| s.name.as_deref())
                    .is_some_and(|n| n != hops[i].name);
                if name_changed || self.is_junction(hops[i].at, mode) {
                    let maneuver = Maneuver::from_turn(signed_turn(
                        bearing_of(smoothed_dir(&hops[..i], true)),
                        bearing_of(smoothed_dir(&hops[i..], false)),
                    ));
                    if maneuver != Maneuver::Continue || name_changed {
                        steps.push(RouteStep {
                            maneuver,
                            name: Some(hops[i].name.clone()),
                            distance_m: 0.0,
                            duration_s: 0.0,
                            at: hops[i].coord,
                        });
                    }
                }
            }
            let cur = steps.last_mut().expect("seeded with the Depart step");
            cur.distance_m += hops[i].len_m;
            cur.duration_s += hops[i].duration_s;
        }

        merge_short_steps(&mut steps);
        steps.push(RouteStep {
            maneuver: Maneuver::Arrive,
            name: None,
            distance_m: 0.0,
            duration_s: 0.0,
            at: *points.last().unwrap(),
        });

        Route {
            mode,
            points,
            levels,
            distance_m,
            duration_s,
            steps,
        }
    }

    /// Does the network branch here, for this mode? Three or more distinct
    /// neighbours means a choice was available, which is the only place an
    /// instruction earns its keep.
    fn is_junction(&self, node: u32, mode: TravelMode) -> bool {
        let mut seen: [u32; 3] = [u32::MAX; 3];
        let mut n = 0;
        for e in &self.edges[node as usize] {
            if self.edge_cost(e, mode).is_none() || seen[..n].contains(&e.to) {
                continue;
            }
            seen[n] = e.to;
            n += 1;
            if n == 3 {
                return true;
            }
        }
        false
    }
}

/// Seconds lost passing through a junction, arriving on a road of `kind`.
///
/// Two factors on [`JUNCTION_S`]. The road's own priority
/// ([`RoadKind::junction_share`]) is what makes an arterial beat a parallel grid
/// of side streets over the same ground. The mode is the other half: a driver
/// waits for a gap, a cyclist slows, and a pedestrian at a side road barely
/// breaks step — charging all three the same would push walking routes onto the
/// big roads, which is precisely backwards.
fn junction_delay(kind: RoadKind, mode: TravelMode) -> f64 {
    let by_mode = match mode {
        TravelMode::Car => 1.0,
        TravelMode::Bike => 0.35,
        TravelMode::Foot => 0.15,
    };
    JUNCTION_S * kind.junction_share() * by_mode
}

/// One traversed edge, with the facts the turn list is built from.
struct Hop {
    /// Node the hop starts at.
    at: u32,
    len_m: f64,
    duration_s: f64,
    name: String,
    /// Unit direction on the lattice.
    dir: (f64, f64),
    /// Start of the hop as `[lat, lng]`.
    coord: [f64; 2],
}

/// Average direction over [`TURN_WINDOW_M`] of `hops`, taken from the end when
/// `from_end` (the approach to a junction) and from the start otherwise (the
/// departure from it). Length-weighted, so a 1 m stub cannot outvote a 40 m street.
fn smoothed_dir(hops: &[Hop], from_end: bool) -> (f64, f64) {
    let mut acc = (0.0, 0.0);
    let mut run = 0.0;
    let ordered: Box<dyn Iterator<Item = &Hop>> = if from_end {
        Box::new(hops.iter().rev())
    } else {
        Box::new(hops.iter())
    };
    for h in ordered {
        acc = (acc.0 + h.dir.0 * h.len_m, acc.1 + h.dir.1 * h.len_m);
        run += h.len_m;
        if run >= TURN_WINDOW_M {
            break;
        }
    }
    acc
}

/// Compass bearing of a lattice direction (0 = north, 90 = east); +y runs south,
/// hence the negation.
fn bearing_of(d: (f64, f64)) -> f64 {
    d.0.atan2(-d.1).to_degrees()
}

/// Fold away steps shorter than [`MIN_STEP_M`] by adding them to the one before.
///
/// What survives the junction filter is still, in a dense city centre, a scatter of
/// 10 m hops across a square modelled as five little ways. Merging them costs the
/// odd genuine "left then immediately right" — which is how a driver reads that
/// pair anyway — and buys a list that can be followed.
fn merge_short_steps(steps: &mut Vec<RouteStep>) {
    let mut i = 1;
    while i < steps.len() {
        if steps[i].distance_m < MIN_STEP_M {
            let (d, t) = (steps[i].distance_m, steps[i].duration_s);
            steps[i - 1].distance_m += d;
            steps[i - 1].duration_s += t;
            steps.remove(i);
        } else {
            i += 1;
        }
    }
}

// ─── Route result ────────────────────────────────────────────────────────────

/// A computed route: the line to draw, the totals to show, and the turn list.
#[derive(Debug, Clone)]
pub struct Route {
    pub mode: TravelMode,
    /// The path as `[lat, lng]`, including the lead-in/out to the requested points.
    pub points: Vec<[f64; 2]>,
    /// The grade each point sits at, parallel to `points`: 0 on the ground, 1 or
    /// more on a bridge, negative in a tunnel.
    ///
    /// Here because the drawn line has to climb onto a viaduct it was routed over,
    /// and *not* climb onto one it was routed under. Those two are the same ground
    /// position, so nothing downstream can recover the difference — the graph is the
    /// last place that knows it (`systems::navigation`, `map_geometry::DeckPlates`).
    pub levels: Vec<i8>,
    pub distance_m: f64,
    pub duration_s: f64,
    pub steps: Vec<RouteStep>,
}

/// One instruction in the turn list.
#[derive(Debug, Clone)]
pub struct RouteStep {
    pub maneuver: Maneuver,
    /// Street name, or a generic stand-in when the tile carries none.
    pub name: Option<String>,
    /// How far this step runs before the next one.
    pub distance_m: f64,
    pub duration_s: f64,
    /// Where the manoeuvre happens — what the camera flies to when a step in the
    /// turn list is clicked.
    pub at: [f64; 2],
}

impl RouteStep {
    /// The instruction as a sentence, Organic-Maps style ("Turn left onto …").
    #[must_use]
    pub fn instruction(&self) -> String {
        let verb = self.maneuver.verb();
        match (&self.name, self.maneuver) {
            (_, Maneuver::Arrive) => "Arrive at your destination".to_string(),
            (Some(name), Maneuver::Depart) => format!("Head out on {name}"),
            (Some(name), Maneuver::Continue) => format!("Continue on {name}"),
            (Some(name), _) => format!("{verb} onto {name}"),
            (None, _) => verb.to_string(),
        }
    }
}

/// What the traveller does at a step boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Maneuver {
    Depart,
    Continue,
    SlightLeft,
    Left,
    SharpLeft,
    SlightRight,
    Right,
    SharpRight,
    UTurn,
    Arrive,
}

impl Maneuver {
    /// Classify a signed turn angle in degrees (negative = left).
    #[must_use]
    pub fn from_turn(deg: f64) -> Self {
        match deg {
            d if d <= -160.0 || d >= 160.0 => Self::UTurn,
            d if d < -110.0 => Self::SharpLeft,
            d if d < -35.0 => Self::Left,
            d if d < -18.0 => Self::SlightLeft,
            d if d > 110.0 => Self::SharpRight,
            d if d > 35.0 => Self::Right,
            d if d > 18.0 => Self::SlightRight,
            _ => Self::Continue,
        }
    }

    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            Self::Depart => "Head out",
            Self::Continue => "Continue",
            Self::SlightLeft => "Bear left",
            Self::Left => "Turn left",
            Self::SharpLeft => "Turn sharp left",
            Self::SlightRight => "Bear right",
            Self::Right => "Turn right",
            Self::SharpRight => "Turn sharp right",
            Self::UTurn => "Make a U-turn",
            Self::Arrive => "Arrive",
        }
    }

    // The arrow each of these is drawn with lives in the client
    // (`cartopolis::systems::icons::maneuver`), not here: it is a codepoint in a
    // font that crate bundles, and this one has no business knowing which. What
    // belongs here is the *distinction* — a sharp left is not a left — and that
    // is what the enum carries.
}

/// Why a route could not be produced. Each variant maps to a message the planner
/// shows verbatim — a router that fails silently is worse than no router.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteError {
    /// No tiles could be read at all (offline, and nothing cached).
    NoData,
    /// Nothing the mode can use near the start.
    NoStart,
    /// Nothing the mode can use near the destination.
    NoEnd,
    /// Both ends snapped, but the network does not connect them — usually the
    /// corridor is too narrow, or the mode is barred from the only link.
    NoPath,
    /// Start and destination snap to the same point on the network.
    TooClose,
    /// The endpoints are too far apart for street-level routing — the corridor
    /// would exceed [`MAX_ROUTE_TILES`]. Carries the straight-line distance in
    /// kilometres, which is what a person can act on (the tile count cannot).
    TooFar { km: f64 },
    /// A tile fetch failed.
    Fetch(String),
}

/// Roughly how far apart two points can be before even the coarse corridor
/// ([`LONG_HAUL_ZOOM`]) exceeds [`MAX_ROUTE_TILES`]. Pinned by a test rather than
/// derived, because the gate is a count of grid cells and not an area.
pub const MAX_STAGED_KM: f64 = 360.0;

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoData => write!(f, "No map data for this area yet"),
            Self::NoStart => write!(f, "No road near the starting point"),
            Self::NoEnd => write!(f, "No road near the destination"),
            Self::NoPath => write!(f, "No route found for this travel mode"),
            Self::TooClose => write!(f, "Start and destination are the same place"),
            Self::TooFar { km } => write!(
                f,
                "Too far to route here — {km:.0} km is beyond the ~{MAX_STAGED_KM:.0} km \
                 reach of the map data one route may pull in"
            ),
            Self::Fetch(e) => write!(f, "Map data unavailable: {e}"),
        }
    }
}

// ─── Tile corridor ───────────────────────────────────────────────────────────

/// Which tiles a route between two points needs.
///
/// A **capsule** around the straight line (see [`CORRIDOR_BASE`]) plus a ring
/// around each endpoint. A bounding box would fetch the whole square a diagonal
/// route spans — the capsule is a fraction of that, which is what keeps a
/// cross-town route affordable on a cold cache.
///
/// Refuses early, before building the list, when the endpoints are far enough apart
/// that the corridor cannot fit in `limit` tiles — reported as a distance, since
/// that is the thing the user chose.
pub fn corridor_tiles(
    from: (f64, f64),
    to: (f64, f64),
    limit: usize,
) -> Result<Vec<(i64, i64)>, RouteError> {
    corridor_tiles_at(ROUTING_ZOOM, from, to, limit)
}

/// [`corridor_tiles`] on an arbitrary tile zoom.
///
/// The capsule shape is the same at every zoom; what changes is that a coarse
/// tile covers 16× the span per axis, so the same ground needs vastly fewer of
/// them. That is the whole mechanism behind long-haul routing — see
/// [`RoadGraph::new_at`] for what the coarse tiles do and do not contain.
pub fn corridor_tiles_at(
    zoom: u32,
    from: (f64, f64),
    to: (f64, f64),
    limit: usize,
) -> Result<Vec<(i64, i64)>, RouteError> {
    let a = lat_lng_to_tile_frac(from.0, from.1, zoom);
    let b = lat_lng_to_tile_frac(to.0, to.1, zoom);
    let direct = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let half_width = (CORRIDOR_BASE + CORRIDOR_SPREAD * direct.sqrt()).min(CORRIDOR_MAX_HALF_WIDTH);

    let (x0, x1) = minmax(a.0, b.0);
    let (y0, y1) = minmax(a.1, b.1);
    let pad = (half_width.max(ENDPOINT_RING) + 1.0).ceil() as i64;

    // Refuse *before* the loop, which the doc comment above has always promised
    // and the code did not do: the loop walked the full bounding box of the two
    // endpoints and only counted the survivors afterwards. A cross-planet query
    // (Amsterdam→Sydney at `ROUTING_ZOOM` is ~6,600 × ~5,000 tiles) therefore ran
    // ~3·10⁷ iterations of `hypot` + `point_seg_distance` on the calling thread —
    // a multi-second stall — to produce a `TooFar` derivable from the endpoints.
    //
    // The test is a genuine *lower* bound on what the loop would collect, so it
    // can never refuse a route the loop would have accepted: the capsule contains
    // the segment, and a segment spanning `span` tiles along its longer axis
    // enters at least that many distinct tiles.
    let span = (b.0 - a.0).abs().max((b.1 - a.1).abs());
    if span > limit as f64 {
        let mid_lat = (from.0 + to.0) / 2.0;
        let km = direct * tile_to_meters(zoom, mid_lat) / 1000.0;
        return Err(RouteError::TooFar { km });
    }

    let mut tiles = Vec::new();
    for tx in (x0.floor() as i64 - pad)..=(x1.floor() as i64 + pad) {
        for ty in (y0.floor() as i64 - pad)..=(y1.floor() as i64 + pad) {
            let c = (tx as f64 + 0.5, ty as f64 + 0.5);
            // Inside the capsule, or in the ring that guarantees each endpoint has
            // streets around it to snap onto even when the capsule is degenerate.
            let near_end = (c.0 - a.0).hypot(c.1 - a.1) <= ENDPOINT_RING + 0.75
                || (c.0 - b.0).hypot(c.1 - b.1) <= ENDPOINT_RING + 0.75;
            if near_end || point_seg_distance(c, a, b) <= half_width {
                tiles.push((tx, ty));
            }
        }
    }
    if tiles.len() > limit {
        // Straight-line distance for the message: one tile's ground size at the
        // midpoint latitude, times the direct span in tiles.
        let mid_lat = (from.0 + to.0) / 2.0;
        let km = direct * tile_to_meters(zoom, mid_lat) / 1000.0;
        return Err(RouteError::TooFar { km });
    }
    // Nearest-to-the-start first, so a partially-fetched corridor is still useful
    // and the progress the user sees moves in the direction they are going.
    tiles.sort_by(|p, q| {
        let d = |t: &(i64, i64)| (t.0 as f64 + 0.5 - a.0).hypot(t.1 as f64 + 0.5 - a.1);
        d(p).partial_cmp(&d(q)).unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(tiles)
}

// ─── Two-tier planning ───────────────────────────────────────────────────────

/// Which tiles, at which zooms, a route needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Corridor {
    /// One graph at [`ROUTING_ZOOM`] covers the whole route — every route that
    /// fits, which is every city route, and the only shape that existed before.
    Direct(Vec<(i64, i64)>),
    /// Too long for street level: a coarse corridor for the haul, and a detailed
    /// patch at each end so the route still starts and finishes on the actual
    /// street ([`FINE_PATCH_TILES`]).
    ///
    /// **The two patches are separate lists because they must become separate
    /// graphs.** Put both in one graph and its routable core — the largest
    /// strongly-connected component — is whichever patch has more streets in it;
    /// the other end is then outside the core, cannot be snapped to, and every
    /// staged route silently falls back to the coarse one. That is precisely what
    /// the first cut did, and the only symptom was the endpoint snap staying at
    /// its coarse 281 m.
    Staged {
        start: Vec<(i64, i64)>,
        end: Vec<(i64, i64)>,
        coarse: Vec<(i64, i64)>,
    },
}

/// Decide how to route between two points, and which tiles that needs.
///
/// Street level if it fits, two tiers if it does not, and a refusal only when even
/// the coarse corridor is out of budget (~380 km, from
/// [`LONG_HAUL_ZOOM`] against [`MAX_ROUTE_TILES`]).
pub fn plan_corridor(from: (f64, f64), to: (f64, f64)) -> Result<Corridor, RouteError> {
    if let Ok(tiles) = corridor_tiles(from, to, MAX_ROUTE_TILES) {
        return Ok(Corridor::Direct(tiles));
    }
    // The coarse corridor is the gate now, so its refusal is the one the user
    // sees — and it speaks in the same "too far, here is how far" terms.
    let coarse = corridor_tiles_at(LONG_HAUL_ZOOM, from, to, MAX_ROUTE_TILES)?;
    Ok(Corridor::Staged {
        start: endpoint_patch(from),
        end: endpoint_patch(to),
        coarse,
    })
}

/// The disc of [`ROUTING_ZOOM`] tiles kept around one endpoint of a staged route.
fn endpoint_patch(at: (f64, f64)) -> Vec<(i64, i64)> {
    let c = lat_lng_to_tile_frac(at.0, at.1, ROUTING_ZOOM);
    let r = FINE_PATCH_TILES.ceil() as i64;
    let mut tiles = Vec::new();
    for tx in (c.0.floor() as i64 - r)..=(c.0.floor() as i64 + r) {
        for ty in (c.1.floor() as i64 - r)..=(c.1.floor() as i64 + r) {
            let d = (tx as f64 + 0.5 - c.0).hypot(ty as f64 + 0.5 - c.1);
            if d <= FINE_PATCH_TILES {
                tiles.push((tx, ty));
            }
        }
    }
    tiles
}

/// Route across two tiers: the coarse graph for the haul, the detailed one for
/// the first and last kilometre.
///
/// The two graphs are **not** merged. Their lattices belong to different zooms, so
/// there is no shared node identity to weld on, and the generalised geometry sits
/// tens of metres from the detailed geometry of the same road — far outside the
/// sub-metre tolerance [`RoadGraph::finish`] nodes with. Welding would mean
/// inventing transfer edges and letting the search hop between tiers wherever it
/// liked. Instead each graph answers the question it is good at and the *routes*
/// are spliced, which fails visibly (a seam) rather than subtly (a route that
/// zig-zags between two representations of one road).
///
/// Falls back to the coarse route whole whenever an end cannot be refined — an
/// approximate route is worth more than an error, and the caller cannot do
/// anything with the distinction.
pub fn route_staged(
    start_patch: &RoadGraph,
    end_patch: &RoadGraph,
    coarse: &RoadGraph,
    from: (f64, f64),
    to: (f64, f64),
    mode: TravelMode,
) -> Result<Route, RouteError> {
    let long = coarse.route(from, to, mode)?;
    // `points` is `[from pin, road…, to pin]`; the handovers are road vertices.
    let n = long.points.len();
    if n < 4 {
        return Ok(long);
    }
    let Some(a) = (1..n - 1).find(|&i| ground_m(long.points[i], long.points[0]) > HANDOVER_M)
    else {
        return Ok(long);
    };
    let Some(b) = (1..n - 1)
        .rev()
        .find(|&i| ground_m(long.points[i], long.points[n - 1]) > HANDOVER_M)
    else {
        return Ok(long);
    };
    if a >= b {
        return Ok(long);
    }

    let (Ok(head), Ok(tail)) = (
        start_patch.route(from, (long.points[a][0], long.points[a][1]), mode),
        end_patch.route((long.points[b][0], long.points[b][1]), to, mode),
    ) else {
        return Ok(long);
    };

    // Geometry: head ends at the handover, the coarse middle carries on from the
    // vertex after it, and the tail begins at the far handover.
    let mut points = head.points.clone();
    points.extend_from_slice(&long.points[a + 1..b]);
    points.extend_from_slice(&tail.points);
    // Grades splice exactly as the geometry does — they are parallel arrays, and a
    // pair that drifts apart is a route line that climbs the wrong viaduct.
    let mut levels = head.levels.clone();
    levels.extend_from_slice(&long.levels[a + 1..b]);
    levels.extend_from_slice(&tail.levels);

    // The coarse middle's own length, measured off the polyline rather than taken
    // from the planner's total — the two legs it replaces are gone. Its time is
    // that share of the coarse route's time, which is the best available: the
    // coarse steps are not indexed by polyline position.
    let middle_m: f64 = long.points[a..=b]
        .windows(2)
        .map(|w| ground_m(w[0], w[1]))
        .sum();
    let middle_s = if long.distance_m > 1.0 {
        long.duration_s * middle_m / long.distance_m
    } else {
        0.0
    };

    // Steps: the fine legs keep theirs (less the Arrive that is not an arrival and
    // the Depart that is not a departure), and the coarse route contributes the
    // ones that fall outside both handover radii. Selecting them by distance from
    // the pins rather than by polyline index is crude and adequate — a route long
    // enough to be staged does not come back within `HANDOVER_M` of its own ends.
    let mut steps = head.steps;
    steps.pop();
    steps.extend(long.steps.into_iter().filter(|s| {
        ground_m(s.at, long.points[0]) > HANDOVER_M
            && ground_m(s.at, long.points[n - 1]) > HANDOVER_M
    }));
    steps.extend(tail.steps.into_iter().skip(1));
    merge_short_steps(&mut steps);

    Ok(Route {
        mode,
        points,
        levels,
        distance_m: head.distance_m + middle_m + tail.distance_m,
        duration_s: head.duration_s + middle_s + tail.duration_s,
        steps,
    })
}

/// Ground metres between two `[lat, lng]`, on the local tangent plane.
fn ground_m(a: [f64; 2], b: [f64; 2]) -> f64 {
    let mid = (a[0] + b[0]) * 0.5;
    let dx = (b[1] - a[1]) * 111_320.0 * mid.to_radians().cos();
    let dy = (b[0] - a[0]) * 110_540.0;
    dx.hypot(dy)
}

// ─── Formatting helpers (shared by the panel and the tests) ──────────────────

/// "450 m" / "4.3 km" — Organic Maps' distance style.
#[must_use]
pub fn format_distance(m: f64) -> String {
    // Round first, *then* pick the unit. Branching on the raw metres and rounding
    // afterwards printed "1000 m" for anything in [995, 1000).
    let rounded = (m / 10.0).round() * 10.0;
    if rounded < 1000.0 {
        format!("{rounded:.0} m")
    } else if m < 10_000.0 {
        format!("{:.1} km", m / 1000.0)
    } else {
        format!("{:.0} km", m / 1000.0)
    }
}

/// "8 min" / "1 h 12 min".
#[must_use]
pub fn format_duration(s: f64) -> String {
    let mins = (s / 60.0).round() as i64;
    if mins < 60 {
        format!("{} min", mins.max(1))
    } else {
        format!("{} h {:02} min", mins / 60, mins % 60)
    }
}

// ─── Small geometry / property helpers ───────────────────────────────────────

/// WGS84 → global lattice units at a tile `zoom`.
fn lattice_at(zoom: u32, lat: f64, lng: f64) -> (f64, f64) {
    let (tx, ty) = lat_lng_to_tile_frac(lat, lng, zoom);
    (tx * EXTENT, ty * EXTENT)
}

fn str_prop<'a>(props: Option<&'a HashMap<String, Value>>, key: &str) -> Option<&'a str> {
    match props?.get(key)? {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}

fn bool_prop(props: Option<&HashMap<String, Value>>, key: &str) -> bool {
    matches!(props.and_then(|p| p.get(key)), Some(Value::Bool(true)))
}

/// An integer-valued property, in whichever of MVT's numeric shapes the encoder
/// chose. Reading only one of them is how `housenumber` lost most of its values
/// (see `vector_tiles`); `layer` is the same tag in the same position to make that
/// mistake in, and a dropped `layer` reads as a viaduct lying on the ground.
fn int_prop(props: Option<&HashMap<String, Value>>, key: &str) -> Option<i64> {
    match props?.get(key)? {
        Value::Int(v) | Value::SInt(v) => Some(*v),
        Value::UInt(v) => Some(*v as i64),
        Value::Float(v) => Some(*v as i64),
        Value::Double(v) => Some(*v as i64),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn minmax(a: f64, b: f64) -> (f64, f64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn bucket(v: f64) -> i32 {
    (v / BUCKET).floor() as i32
}

/// The inclusive bucket range a lattice span covers, or `None` when it is wider
/// than a segment of one tile's geometry could possibly be.
///
/// This is the backstop for the coordinate window in [`RoadGraph::add_tile`], and
/// it exists because the failure it guards against is not a wrong answer but an
/// unbounded amount of work: [`bucket`] casts an `f64` to `i32`, and a float cast
/// **saturates**, so a coordinate near 2.1e9 — all `mvt-reader` promises, since it
/// clips its cursor at `i32::MAX` and never to the tile extent — yields
/// `i32::MAX` and a loop of ~4·10^9 per axis, each iteration of which either
/// allocates a `Vec` (the label index) or probes a `HashMap` (the noding pass).
///
/// [`MAX_SEG_BUCKETS`] is ~16,000 lattice units, four tiles' worth, against the
/// ~5,100 a tile plus its buffer can span at any zoom — headroom of 3×, which is
/// the right shape for a guard nothing legitimate should ever meet.
fn bucket_span(lo: f64, hi: f64) -> Option<(i32, i32)> {
    let (a, b) = (bucket(lo), bucket(hi));
    if i64::from(b) - i64::from(a) > MAX_SEG_BUCKETS {
        return None;
    }
    Some((a, b))
}

/// How many buckets one segment's bounding box may span per axis before
/// [`bucket_span`] refuses it.
const MAX_SEG_BUCKETS: i64 = 64;

/// Turn from bearing `from` to bearing `to`, normalised to (−180, 180]; negative
/// is a left turn.
fn signed_turn(from: f64, to: f64) -> f64 {
    let mut d = to - from;
    while d <= -180.0 {
        d += 360.0;
    }
    while d > 180.0 {
        d -= 360.0;
    }
    d
}

/// Distance from `p` to segment `a→b`, and the clamped parameter of the closest
/// point along it (0 at `a`, 1 at `b`) — the noding pass needs both.
fn point_seg_projection(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let len2 = vx * vx + vy * vy;
    if len2 <= f64::EPSILON {
        return (((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt(), 0.0);
    }
    let t = (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / len2).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + t * vx, a.1 + t * vy);
    (((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt(), t)
}

fn point_seg_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    point_seg_projection(p, a, b).0
}

/// `f64` wrapper with a total order, so the priority queue can hold costs.
#[derive(PartialEq, PartialOrd)]
struct Ordered(f64);
impl Eq for Ordered {}
#[allow(clippy::derive_ord_xor_partial_ord)]
impl Ord for Ordered {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Costs are finite by construction (lengths and speeds are), so the
        // fallback is unreachable rather than a silently wrong ordering.
        self.0
            .partial_cmp(&other.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// A min-heap entry: `BinaryHeap` is a max-heap, so the ordering is reversed.
struct Candidate {
    priority: Ordered,
    node: u32,
}
impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.priority.cmp(&self.priority)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_tiles::synth;

    /// The same real z14 Groningen tile the renderer tests use.
    const GRONINGEN_Z14: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/groningen_z14.mvt"
    ));
    /// Its tile coordinates (see `vector_tiles::tests`).
    const TILE: (i64, i64) = (8490, 5319);

    fn graph() -> RoadGraph {
        let mut g = RoadGraph::new(53.2194);
        g.add_tile(&TileData::parse(GRONINGEN_Z14).expect("parse"), TILE)
            .expect("add tile");
        g.finish();
        g
    }

    /// The centre of the fixture tile, and a point near its north-west quarter —
    /// both well inside Groningen's street network.
    fn tile_point(fx: f64, fy: f64) -> (f64, f64) {
        let n = 2.0_f64.powi(ROUTING_ZOOM as i32);
        let x = TILE.0 as f64 + fx;
        let y = TILE.1 as f64 + fy;
        let lng = x / n * 360.0 - 180.0;
        let lat = ((1.0 - 2.0 * y / n) * std::f64::consts::PI)
            .sinh()
            .atan()
            .to_degrees();
        (lat, lng)
    }

    /// Add one segment straight to the raw list, bypassing the tile decode — the
    /// way to build a graph whose shape is the thing under test.
    fn seg(g: &mut RoadGraph, a: (f64, f64), b: (f64, f64), kind: RoadKind) {
        seg_at(g, a, b, kind, 0);
    }

    /// As [`seg`], at an explicit grade.
    fn seg_at(g: &mut RoadGraph, a: (f64, f64), b: (f64, f64), kind: RoadKind, level: i8) {
        line_at(g, &[a, b], kind, level);
    }

    /// Add a whole polyline the way a tile's line arrives: its two ends are dangles
    /// and every vertex between them is interior.
    ///
    /// That distinction is what `finish` routes on, so a test about crossings has to
    /// be built out of lines and not out of segments — a chain of [`seg_at`] calls
    /// marks every joint a dangle and quietly tests the opposite thing.
    fn line_at(g: &mut RoadGraph, pts: &[(f64, f64)], kind: RoadKind, level: i8) {
        let ids: Vec<u32> = pts.iter().map(|&p| g.node(p, level)).collect();
        for w in ids.windows(2) {
            g.raw.push(RawSeg {
                a: w[0],
                b: w[1],
                kind,
                fwd_blocked: false,
                rev_blocked: false,
                bike_denied: false,
                unpaved: false,
                level,
            });
        }
        g.node_terminal[ids[0] as usize] = true;
        g.node_terminal[*ids.last().expect("a line has at least one vertex") as usize] = true;
    }

    /// A viaduct and the street beneath it are not a junction, however exactly the
    /// noding pass finds one lying on the other.
    ///
    /// The geometry is the one that bites: the bridge carries a *vertex* at the
    /// crossing (its own generalisation put one there) while the ground road runs
    /// straight through, so the sweep sees a node sitting on a segment and — before
    /// the grade guard — spliced them into a slip road nobody can drive.
    #[test]
    fn a_bridge_does_not_join_the_street_it_crosses() {
        let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
        let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

        let mut g = RoadGraph::new(53.2194);
        // The street, straight through, with no vertex at the crossing.
        seg(&mut g, at(0.0, 0.0), at(800.0, 0.0), RoadKind::Residential);
        // The viaduct over it, hinged exactly on the street's centre line.
        seg_at(
            &mut g,
            at(400.0, -300.0),
            at(400.0, 0.0),
            RoadKind::Primary,
            1,
        );
        seg_at(
            &mut g,
            at(400.0, 0.0),
            at(400.0, 300.0),
            RoadKind::Primary,
            1,
        );
        g.finish();

        assert_eq!(g.cross_level_refused, 1, "the guard did not fire");
        let street = g.index[&(base.0 as i32, base.1 as i32)];
        let on_the_bridge = g.index[&((base.0 + 400.0) as i32, (base.1 + 300.0) as i32)];
        assert!(
            !g.reachable(street, TravelMode::Car)
                .contains(&on_the_bridge),
            "a car drove off the street and onto the viaduct crossing over it"
        );
    }

    /// …and it still meets the ground at its abutments.
    ///
    /// Same shape, one difference that is the whole distinction available in this
    /// data: here the *ground road* carries the vertex too, because the two really
    /// do share an OSM node. That shared vertex is a shared graph node, so the
    /// junction exists without the noding pass being asked — which is why
    /// [`RoadGraph::node`] keys on the coordinate alone. Key the level in and every
    /// bridge in the network is severed from both its ends; this test is what
    /// notices.
    #[test]
    fn a_bridge_meets_the_ground_at_its_abutments() {
        let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
        let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

        let mut g = RoadGraph::new(53.2194);
        seg(&mut g, at(0.0, 0.0), at(400.0, 0.0), RoadKind::Residential);
        seg(&mut g, at(400.0, 0.0), at(800.0, 0.0), RoadKind::Residential);
        seg_at(
            &mut g,
            at(400.0, 0.0),
            at(400.0, 300.0),
            RoadKind::Primary,
            1,
        );
        g.finish();

        let street = g.index[&(base.0 as i32, base.1 as i32)];
        let on_the_bridge = g.index[&((base.0 + 400.0) as i32, (base.1 + 300.0) as i32)];
        assert!(
            g.reachable(street, TravelMode::Car).contains(&on_the_bridge),
            "the ramp is unreachable from the road it ramps off"
        );
    }

    /// Both guards fire on real tiles, and the grade one fires on levels that came
    /// out of the attributes rather than out of a default.
    ///
    /// The second assertion is the one that fails if Shortbread renames `bridge`, or
    /// if a decode change drops the flag: `level_of` would answer 0 everywhere,
    /// every grade refusal would disappear, and that guard would sit inert while
    /// looking installed. Measured 2026-08-20 on the Groningen z14 fixture: 59
    /// mid-span and 2 cross-level junctions refused, 6348 → 6226 undirected edges,
    /// car component share 0.9199 → 0.9195. The grade count is low because
    /// Groningen's bridges mostly cross *water*, where there is no road to be fused
    /// with; `docs/notes/grade-separation.md` has the full A/B.
    #[test]
    fn both_noding_guards_bite_on_real_tiles() {
        let g = graph();
        assert!(
            g.mid_span_refused > 0,
            "no crossing was refused over a whole city tile — the dangle guard is \
             not seeing which vertices end a line"
        );
        assert!(
            g.cross_level_refused > 0,
            "no cross-level junction was refused over a whole city tile — the \
             bridge/tunnel attributes are not reaching the graph"
        );
    }

    /// `layer` decides where the tile carries one, the flags decide where it does
    /// not — and Shortbread is the second case, so the flags are what does the work
    /// today (its `streets` layer ships `bridge` and `tunnel` and no `layer`,
    /// checked 2026-08-20 on the fixture). The consequence is worth knowing: every
    /// bridge lands on level 1, which separates a viaduct from the ground beneath it
    /// but says nothing about a second viaduct crossing it at an interchange — those
    /// are told apart by the dangle rule instead
    /// (`two_viaducts_crossing_are_not_a_junction`).
    #[test]
    fn a_level_comes_from_layer_first_and_the_flags_after() {
        let props = |pairs: &[(&str, Value)]| -> HashMap<String, Value> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect()
        };

        let flat = props(&[]);
        assert_eq!(level_of(Some(&flat)), 0);

        let bridge = props(&[("bridge", Value::Bool(true))]);
        assert_eq!(level_of(Some(&bridge)), 1);

        let tunnel = props(&[("tunnel", Value::Bool(true))]);
        assert_eq!(level_of(Some(&tunnel)), -1);

        // An explicit `layer` outranks the flag, so a stack of decks separates.
        let upper = props(&[("bridge", Value::Bool(true)), ("layer", Value::SInt(2))]);
        assert_eq!(level_of(Some(&upper)), 2);

        // …in whichever numeric shape the encoder picked. Reading only one of them
        // is how `housenumber` lost most of its values.
        for v in [
            Value::Int(-2),
            Value::SInt(-2),
            Value::Float(-2.0),
            Value::Double(-2.0),
            Value::String("-2".into()),
        ] {
            let sunk = props(&[("layer", v.clone())]);
            assert_eq!(level_of(Some(&sunk)), -2, "layer as {v:?}");
        }

        // A `layer` past the mask's window still separates from the ground rather
        // than wrapping onto some other level's bit.
        let absurd = props(&[("layer", Value::SInt(99))]);
        assert_eq!(level_of(Some(&absurd)), MAX_LEVEL);
        assert_ne!(level_bit(MAX_LEVEL), level_bit(0));
    }

    /// A route's grades line up with its geometry, and the abutments read as ground.
    ///
    /// This is what stops the drawn line climbing the wrong viaduct. `points` and
    /// `levels` are parallel arrays and every splice has to keep them so; a route
    /// that crosses *under* a bridge has the same ground coordinates as one crossing
    /// over it, so if these two ever drift apart nothing downstream can notice.
    #[test]
    fn a_route_carries_the_grade_of_the_road_it_is_on() {
        let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
        let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

        let mut g = RoadGraph::new(53.2194);
        // Ground, then a bridge span, then ground again — one continuous road, with
        // its abutments at x = 400 and x = 800 shared between the runs.
        line_at(
            &mut g,
            &[at(0.0, 0.0), at(400.0, 0.0)],
            RoadKind::Residential,
            0,
        );
        line_at(
            &mut g,
            &[at(400.0, 0.0), at(600.0, 0.0), at(800.0, 0.0)],
            RoadKind::Residential,
            1,
        );
        line_at(
            &mut g,
            &[at(800.0, 0.0), at(1200.0, 0.0)],
            RoadKind::Residential,
            0,
        );
        g.finish();

        let ends = |p: (f64, f64)| {
            let c = g.index[&(p.0 as i32, p.1 as i32)] as usize;
            g.lattice_to_lat_lng(g.coords[c].0, g.coords[c].1)
        };
        let route = g
            .route(ends(at(0.0, 0.0)), ends(at(1200.0, 0.0)), TravelMode::Car)
            .expect("one road end to end");

        assert_eq!(
            route.levels.len(),
            route.points.len(),
            "the grades must stay parallel to the geometry"
        );
        // The mid-span vertex is the only one wholly on the structure; both
        // abutments are shared with a ground run and count as ground, which is where
        // the deck's own fascia puts the step too.
        assert!(
            route.levels.contains(&1),
            "the span was routed over but no point reports being on it: {:?}",
            route.levels
        );
        assert_eq!(route.levels[0], 0, "the start pin is on the ground");
        assert_eq!(
            *route.levels.last().expect("levels"),
            0,
            "and so is the finish pin"
        );
    }

    /// Two viaducts crossing each other at an interchange are not a junction — and
    /// this is the case no attribute in the tile can settle, because both of them
    /// are simply `bridge`. Only the dangle rule separates them.
    ///
    /// This was written off as unfixable when the grade guard went in ("nothing in
    /// this repo can recover it"). It was fixable; the recoverable fact was never
    /// the height, it was which line *ends* at the contact.
    #[test]
    fn two_viaducts_crossing_are_not_a_junction() {
        let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
        let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

        let mut g = RoadGraph::new(53.2194);
        // Both decks. Neither carries a vertex the other could be tested against —
        // except that the upper one is hinged over the lower, exactly as a
        // generalised line is.
        line_at(&mut g, &[at(0.0, 0.0), at(800.0, 0.0)], RoadKind::Primary, 1);
        line_at(
            &mut g,
            &[at(400.0, -300.0), at(400.0, 0.0), at(400.0, 300.0)],
            RoadKind::Primary,
            1,
        );
        g.finish();

        assert_eq!(g.mid_span_refused, 1, "the dangle guard did not fire");
        assert_eq!(
            g.cross_level_refused, 0,
            "both decks are level 1 — the grade guard cannot see this one"
        );
        let lower = g.index[&(base.0 as i32, base.1 as i32)];
        let upper = g.index[&((base.0 + 400.0) as i32, (base.1 + 300.0) as i32)];
        assert!(
            !g.reachable(lower, TravelMode::Car).contains(&upper),
            "a car changed decks in mid-air"
        );
    }

    /// The tee the noding pass exists for still joins, which is the half that a
    /// guard is always one step from breaking: refuse a little too much and the
    /// network shatters again, silently, into `NoPath` for ordinary routes.
    #[test]
    fn a_side_street_that_stops_dead_on_a_road_still_joins_it() {
        let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
        let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

        let mut g = RoadGraph::new(53.2194);
        // The through road, generalised past the tee — it has no vertex there.
        line_at(
            &mut g,
            &[at(0.0, 0.0), at(800.0, 0.0)],
            RoadKind::Residential,
            0,
        );
        // The side street, ending on it.
        line_at(
            &mut g,
            &[at(400.0, 0.0), at(400.0, 300.0)],
            RoadKind::Residential,
            0,
        );
        g.finish();

        assert_eq!(g.mid_span_refused + g.cross_level_refused, 0);
        let road = g.index[&(base.0 as i32, base.1 as i32)];
        let up_the_side_street = g.index[&((base.0 + 400.0) as i32, (base.1 + 300.0) as i32)];
        assert!(
            g.reachable(road, TravelMode::Car)
                .contains(&up_the_side_street),
            "the side street is unreachable from the road it tees into"
        );
    }

    /// A/B of the two noding guards on the real fixture: the same tile noded with
    /// each combination of them, against the flat pass they replaced. Diagnostic,
    /// not a gate — the numbers it prints are quoted in
    /// `docs/notes/grade-separation.md`.
    #[test]
    #[ignore]
    fn grade_separation_diagnostic() {
        // Each variant is expressed by *relaxing* the finished graph's inputs
        // before noding, so there is no knob in production code: dropping every
        // level to the ground disables the grade guard, and marking every node a
        // dangle disables the mid-span one.
        let build = |levels: bool, dangles: bool| {
            let mut g = RoadGraph::new(53.2194);
            g.add_tile(&TileData::parse(GRONINGEN_Z14).expect("parse"), TILE)
                .expect("add tile");
            if !levels {
                for s in &mut g.raw {
                    s.level = 0;
                }
                for l in &mut g.node_levels {
                    *l = level_bit(0);
                }
            }
            if !dangles {
                for t in &mut g.node_terminal {
                    *t = true;
                }
            }
            g.finish();
            g
        };

        println!(
            "{:<14} {:>7} {:>9} {:>10} {:>8} {:>8} {:>8}",
            "variant", "edges", "mid-span", "cross-lvl", "car", "bike", "foot"
        );
        for (name, levels, dangles) in [
            ("flat", false, false),
            ("level only", true, false),
            ("dangle only", false, true),
            ("both", true, true),
        ] {
            let g = build(levels, dangles);
            let share = |mode| {
                let (l, u) = g.largest_component(mode);
                l as f64 / u as f64
            };
            println!(
                "{name:<14} {:>7} {:>9} {:>10} {:>8.4} {:>8.4} {:>8.4}",
                g.edge_count(),
                g.mid_span_refused,
                g.cross_level_refused,
                share(TravelMode::Car),
                share(TravelMode::Bike),
                share(TravelMode::Foot),
            );
        }
    }

    /// A vertex no tile can legitimately carry must never reach the graph.
    ///
    /// Tile bytes come from a server we do not run, and `mvt-reader` clips its
    /// coordinate cursor at `i32::MAX` rather than at the tile's extent, so a few
    /// bytes of crafted varint deliver ~2.1e9. What follows is not a wrong route:
    /// `finish`'s noding sweep and the label index are both loops over a
    /// segment's bucket bounding box, and at that scale each is ~10^14 iterations
    /// with a `HashMap` probe or a fresh `Vec` in every one.
    #[test]
    fn a_street_running_off_to_two_billion_never_enters_the_graph() {
        let hostile = synth::tile(&[synth::line_layer(
            "streets",
            4096,
            &[(10, 10), (2_000_000_000, 20), (30, 30)],
        )]);
        let mut g = RoadGraph::new(53.2194);
        g.add_tile(&TileData::parse(&hostile).expect("parse"), TILE)
            .expect("add tile");
        g.finish();
        assert!(
            g.is_empty(),
            "{} nodes interned from a street that leaves the planet",
            g.node_count()
        );

        // …and an ordinary street from the same encoder does enter it, so what
        // was tested above is a window and not a wall.
        let ordinary = synth::tile(&[synth::line_layer("streets", 4096, &[(10, 10), (2000, 20)])]);
        let mut g = RoadGraph::new(53.2194);
        g.add_tile(&TileData::parse(&ordinary).expect("parse"), TILE)
            .expect("add tile");
        g.finish();
        assert_eq!(g.node_count(), 2, "an ordinary street was refused");
        assert_eq!(g.edge_count(), 2, "both traversals of one segment");
    }

    /// The backstop under that window: a bucket sweep must be bounded whatever it
    /// is handed, because `bucket`'s `as i32` saturates rather than wrapping.
    #[test]
    fn the_bucket_span_refuses_what_no_tile_could_span() {
        // A whole tile is 16 buckets, comfortably inside the ceiling.
        assert_eq!(bucket_span(0.0, EXTENT), Some((0, 16)));
        assert!(bucket_span(0.0, 2.1e9).is_none());
        assert!(bucket_span(-2.1e9, 2.1e9).is_none());
    }

    /// The junction cost has to change *route choice*, not just the clock.
    ///
    /// Two ways between the same pair, both residential so the class speed cannot
    /// decide it: a straight run crossed by a side street every 50 m, and a detour
    /// 19% longer with nothing on it. On length alone the straight one wins, which
    /// is the rat-run the old cost model had no way to avoid. Charging for the
    /// junctions flips it — and the test asserts *both* halves, because a penalty
    /// that changes nothing and a penalty that changes everything look the same
    /// from one route.
    #[test]
    fn a_junction_free_detour_beats_a_grid_of_side_streets() {
        // Placed on the fixture tile's patch of lattice so the latitude the graph
        // was scaled for is the latitude the geometry is at.
        let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
        let at = |x: f64, y: f64| (base.0 + x, base.1 + y);
        const SPAN: f64 = 2800.0; // ~1 km at z14
        const STEPS: usize = 20;

        let mut g = RoadGraph::new(53.2194);
        // The straight run, with a stub off every interior vertex.
        for i in 0..STEPS {
            let (x0, x1) = (
                SPAN * i as f64 / STEPS as f64,
                SPAN * (i + 1) as f64 / STEPS as f64,
            );
            seg(&mut g, at(x0, 0.0), at(x1, 0.0), RoadKind::Residential);
            if i > 0 {
                seg(&mut g, at(x0, 0.0), at(x0, 300.0), RoadKind::Residential);
            }
        }
        // The clear detour: two long legs, one interior vertex, no side streets.
        seg(
            &mut g,
            at(0.0, 0.0),
            at(SPAN / 2.0, -900.0),
            RoadKind::Residential,
        );
        seg(
            &mut g,
            at(SPAN / 2.0, -900.0),
            at(SPAN, 0.0),
            RoadKind::Residential,
        );
        g.finish();

        let a = g.lattice_to_lat_lng(base.0, base.1);
        let b = g.lattice_to_lat_lng(base.0 + SPAN, base.1);
        let route = g.route(a, b, TravelMode::Car).expect("a car route");

        // The detour is measurably longer, so taking it can only be the junctions.
        let straight_m = SPAN * g.m_per_unit;
        assert!(
            route.distance_m > straight_m * 1.10,
            "took the grid: {:.0} m against a {:.0} m straight run",
            route.distance_m,
            straight_m
        );

        // …and the same graph with the penalty removed takes the straight run.
        // Asserted through `junction_delay` rather than by rebuilding the graph, so
        // the two answers differ in exactly one thing.
        assert!(
            junction_delay(RoadKind::Residential, TravelMode::Car) > 0.0,
            "the premise of this test is that a residential junction costs something"
        );
        let grid_junctions = (STEPS - 1) as f64;
        let detour_extra_m = route.distance_m - straight_m;
        let detour_extra_s =
            detour_extra_m / (RoadKind::Residential.speed_kmh(TravelMode::Car).unwrap() / 3.6);
        assert!(
            grid_junctions * junction_delay(RoadKind::Residential, TravelMode::Car) > detour_extra_s,
            "the junctions ({grid_junctions} of them) must outweigh the {detour_extra_s:.0} s detour \
             or this test proves nothing"
        );
    }

    /// The junction delay's whole job is the *ranking*: priority roads pay less
    /// than the streets that give way to them, motorways pay nothing at all, and a
    /// pedestrian is barely delayed by a side road. Get the order wrong and the
    /// router pushes walkers onto arterials.
    #[test]
    fn junction_delay_follows_priority_and_mode() {
        use RoadKind::{Motorway, Primary, Residential, Secondary, Tertiary, Trunk};
        let car = |k| junction_delay(k, TravelMode::Car);
        assert_eq!(
            car(Motorway),
            0.0,
            "a motorway has no junctions to give way at"
        );
        assert_eq!(car(Trunk), 0.0);
        assert!(car(Primary) < car(Secondary));
        assert!(car(Secondary) < car(Tertiary));
        assert!(car(Tertiary) < car(Residential));
        for kind in [Primary, Tertiary, Residential] {
            assert!(
                junction_delay(kind, TravelMode::Foot) < junction_delay(kind, TravelMode::Bike),
                "{kind:?}: a walker waits less than a cyclist"
            );
            assert!(
                junction_delay(kind, TravelMode::Bike) < car(kind),
                "{kind:?}: a cyclist waits less than a driver"
            );
        }
    }

    /// Ground kilometres between two `[lat, lng]`, for the reach tests below.
    fn km_between(a: (f64, f64), b: (f64, f64)) -> f64 {
        ground_m([a.0, a.1], [b.0, b.1]) / 1000.0
    }

    /// A point `km` due east of another, near enough for a tangent-plane step.
    fn east_of(from: (f64, f64), km: f64) -> (f64, f64) {
        (
            from.0,
            from.1 + km * 1000.0 / (111_320.0 * from.0.to_radians().cos()),
        )
    }

    /// The unit is picked from the *rounded* number, so nothing prints "1000 m".
    #[test]
    fn a_distance_never_rounds_past_its_own_unit() {
        assert_eq!(format_distance(450.0), "450 m");
        assert_eq!(format_distance(994.0), "990 m");
        // The regression: these round to 1000, which is not a metres value.
        assert_eq!(format_distance(995.0), "1.0 km");
        assert_eq!(format_distance(999.0), "1.0 km");
        assert_eq!(format_distance(1_000.0), "1.0 km");
        assert_eq!(format_distance(4_300.0), "4.3 km");
        assert_eq!(format_distance(42_000.0), "42 km");
    }

    /// A hopeless corridor is refused from the endpoints alone. The old code
    /// walked the endpoints' whole bounding box first — ~3·10⁷ iterations for an
    /// intercontinental pair — to reach the same answer.
    ///
    /// The time budget is an order-of-magnitude guard, not a benchmark: the
    /// rejection is now a handful of arithmetic ops, while the loop it replaces
    /// measured in hundreds of milliseconds.
    #[test]
    fn an_impossible_corridor_is_refused_without_walking_the_bounding_box() {
        let amsterdam = (52.3676, 4.9041);
        let sydney = (-33.8688, 151.2093);

        let started = std::time::Instant::now();
        let refused = corridor_tiles_at(ROUTING_ZOOM, amsterdam, sydney, MAX_ROUTE_TILES);
        let elapsed = started.elapsed();

        assert!(
            matches!(refused, Err(RouteError::TooFar { .. })),
            "an intercontinental corridor must be refused, got {refused:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "the refusal walked the bounding box: took {elapsed:?}"
        );

        // …and the early test must not refuse anything the loop would have taken:
        // a corridor that fits is still built. (`the_planner_steps_down_a_tier…`
        // sweeps this boundary properly; this is the cheap direct check.)
        let here = (53.2194, 6.5665);
        assert!(
            corridor_tiles_at(ROUTING_ZOOM, here, east_of(here, 8.0), MAX_ROUTE_TILES).is_ok(),
            "a city-scale corridor was refused by the early bound"
        );
    }

    /// Street level while it fits, two tiers once it does not, and a refusal only
    /// past the coarse budget. The thresholds are the product, not the mechanism:
    /// a change that quietly moved either would change which routes exist.
    #[test]
    fn the_planner_steps_down_a_tier_rather_than_refusing() {
        let here = (53.2194, 6.5665);

        // Well inside street-level reach.
        assert!(
            matches!(
                plan_corridor(here, east_of(here, 8.0)),
                Ok(Corridor::Direct(_))
            ),
            "a city route must stay on one street-level graph"
        );

        // Past it — the old hard stop, now a staged route.
        let long = east_of(here, 60.0);
        let Ok(Corridor::Staged { start, end, coarse }) = plan_corridor(here, long) else {
            panic!("60 km should stage, not refuse");
        };
        assert!(!start.is_empty() && !end.is_empty() && !coarse.is_empty());
        assert!(
            coarse.len() <= MAX_ROUTE_TILES,
            "the coarse corridor is the budget now: {} tiles",
            coarse.len()
        );
        // The patches are separate lists *and* must not have been merged: at this
        // range they cannot overlap, and sharing one graph is the bug that made
        // every staged route fall back (see `Corridor::Staged`).
        assert!(
            start.iter().all(|t| !end.contains(t)),
            "the endpoint patches ran into each other"
        );

        // The transition is where `MAX_ROUTE_KM` says it is, give or take the
        // corridor being a count of grid cells rather than a smooth area.
        let direct_reach = (1..)
            .map(|i| f64::from(i) * 2.0)
            .take_while(|&km| {
                matches!(
                    plan_corridor(here, east_of(here, km)),
                    Ok(Corridor::Direct(_))
                )
            })
            .last()
            .expect("some distance routes directly");
        assert!(
            (direct_reach - MAX_ROUTE_KM).abs() < 2.0,
            "street-level reach measured {direct_reach:.0} km against MAX_ROUTE_KM {MAX_ROUTE_KM:.0}"
        );

        // …and the staged reach is what `MAX_STAGED_KM` claims, which is the
        // number the refusal message quotes at the user.
        let staged_reach = (1..)
            .map(|i| f64::from(i) * 10.0)
            .take_while(|&km| plan_corridor(here, east_of(here, km)).is_ok())
            .last()
            .expect("some distance stages");
        // Both sampled on their own step, so the true limits sit inside one step
        // above these; the constants name the guaranteed reach, not the ceiling.
        assert!(
            (staged_reach - MAX_STAGED_KM).abs() < 10.0,
            "staged reach measured {staged_reach:.0} km against MAX_STAGED_KM {MAX_STAGED_KM:.0}"
        );

        // Past everything, the refusal reports a distance the user can act on.
        let Err(RouteError::TooFar { km }) = plan_corridor(here, east_of(here, 900.0)) else {
            panic!("900 km must be refused");
        };
        assert!(
            (km - km_between(here, east_of(here, 900.0))).abs() < 60.0,
            "reported {km:.0} km"
        );
    }

    /// A ferry carries walkers and cyclists and — deliberately — not cars. The
    /// tile says only `kind = ferry`; every car ferry takes foot passengers and no
    /// foot ferry takes cars, so this is the only assignment that cannot send
    /// someone to a crossing they can't board.
    #[test]
    fn a_ferry_carries_everyone_but_the_car() {
        assert_eq!(RoadKind::Ferry.speed_kmh(TravelMode::Car), None);
        assert!(RoadKind::Ferry.speed_kmh(TravelMode::Foot).is_some());
        assert!(RoadKind::Ferry.speed_kmh(TravelMode::Bike).is_some());
        // Its wait is already inside its speed, so it pays no junction delay on
        // top — the slipways would otherwise be charged as give-ways.
        assert_eq!(junction_delay(RoadKind::Ferry, TravelMode::Foot), 0.0);
        // Slower than walking, or the router would treat open water as a shortcut
        // wherever a ferry happens to run parallel to a road.
        let ferry = RoadKind::Ferry.speed_kmh(TravelMode::Foot).unwrap();
        assert!(ferry > RoadKind::Footway.speed_kmh(TravelMode::Foot).unwrap());
        assert!(ferry < RoadKind::Cycleway.speed_kmh(TravelMode::Bike).unwrap());
    }

    /// "No road near the destination" must mean there is no road there — not that
    /// there is a whole town there which this mode cannot reach.
    ///
    /// The case is real and the ferry work is what surfaced it: a car asked to
    /// cross an estuary snapped its destination fine, but the far shore is its own
    /// component (only the ferry links them, and a car may not use one), so the
    /// core snap failed and the planner announced there was no road in Breskens.
    #[test]
    fn an_unreachable_town_is_not_a_town_without_roads() {
        let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
        let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

        let mut g = RoadGraph::new(53.2194);
        // Two street networks, far apart, with nothing between them — and
        // deliberately *different sizes*. The routable core is the largest
        // component, so two equal islands leave which one wins to the order
        // Kosaraju happens to finish in, and the test would be asserting against
        // a coin flip (it did: the first draft passed by reporting NoPath for the
        // *start* being outside the core).
        for x in 0..6 {
            let x = f64::from(x) * 400.0;
            seg(
                &mut g,
                at(x, 0.0),
                at(x + 400.0, 0.0),
                RoadKind::Residential,
            );
        }
        seg(
            &mut g,
            at(20_000.0, 0.0),
            at(20_400.0, 0.0),
            RoadKind::Residential,
        );
        g.finish();

        let here = g.lattice_to_lat_lng(base.0, base.1);
        let across = g.lattice_to_lat_lng(base.0 + 20_000.0, base.1);
        assert_eq!(
            g.route(here, across, TravelMode::Car).err(),
            Some(RouteError::NoPath),
            "a road that exists but does not connect is NoPath, not NoEnd"
        );

        // …and somewhere with genuinely no road still reports the absence.
        let empty = g.lattice_to_lat_lng(base.0 + 3000.0, base.1 + 40_000.0);
        assert_eq!(
            g.route(here, empty, TravelMode::Car).err(),
            Some(RouteError::NoEnd)
        );
    }

    /// A* is only correct while the heuristic never over-estimates. Tightening it
    /// to the graph's own fastest edge is the whole speed-up, and it is also
    /// exactly where admissibility could be lost — so check it against every edge
    /// in a real graph rather than by reading the code.
    #[test]
    fn the_heuristic_never_overestimates() {
        let g = graph();
        for mode in TravelMode::ALL {
            let top = g.top_speed_ms(mode);
            for edge in g.edges.iter().flatten() {
                let Some(kmh) = edge.kind.speed_kmh(mode) else {
                    continue;
                };
                assert!(
                    kmh / 3.6 <= top + 1e-9,
                    "{mode:?}: {:?} runs at {kmh} km/h, above the heuristic's {:.1} km/h ceiling",
                    edge.kind,
                    top * 3.6
                );
            }
            // …and it must actually be tighter than the mode's global ceiling, or
            // there was no point (the fixture is a city: no 130 km/h anything).
            assert!(
                top < mode.max_speed_ms(),
                "{mode:?}: the graph ceiling is no tighter than the mode's"
            );
        }
    }

    #[test]
    fn builds_a_connected_graph_from_one_tile() {
        let g = graph();
        assert!(g.node_count() > 1_000, "only {} nodes", g.node_count());
        assert!(g.edge_count() > g.node_count(), "graph is not connected up");
    }

    /// Node identity across a tile seam is *exact*, because every tile at a zoom
    /// shares one integer lattice. This is the property the whole stitching model
    /// rests on, so pin it directly rather than trusting the graph counts.
    #[test]
    fn the_global_lattice_is_shared_between_neighbouring_tiles() {
        // A point one unit inside tile (8490, 5319)'s eastern edge, expressed from
        // that tile and from its eastern neighbour's buffer, lands on one integer.
        let from_left = TILE.0 as f64 * EXTENT + 4095.0;
        let from_right = (TILE.0 + 1) as f64 * EXTENT + -1.0;
        assert_eq!(from_left, from_right);
    }

    #[test]
    fn routes_across_the_city_by_car() {
        let g = graph();
        let a = tile_point(0.3, 0.35);
        let b = tile_point(0.7, 0.65);
        let route = g.route(a, b, TravelMode::Car).expect("car route");

        // The straight-line distance between the endpoints, for a sanity band: a
        // real route is longer than the crow flies but not by a factor of three.
        let direct = {
            let pa = RoadGraph::lat_lng_to_lattice(a.0, a.1);
            let pb = RoadGraph::lat_lng_to_lattice(b.0, b.1);
            ((pb.0 - pa.0).powi(2) + (pb.1 - pa.1).powi(2)).sqrt() * g.m_per_unit
        };
        assert!(
            route.distance_m >= direct * 0.9 && route.distance_m < direct * 3.0,
            "route {:.0} m vs direct {:.0} m",
            route.distance_m,
            direct
        );
        assert!(route.duration_s > 0.0);
        assert!(route.points.len() > 4);
        assert!(route.steps.len() >= 2, "expected a turn list");
        assert_eq!(route.steps.last().unwrap().maneuver, Maneuver::Arrive);
        // Steps must add up to the route (the Arrive step carries no distance).
        let stepped: f64 = route.steps.iter().map(|s| s.distance_m).sum();
        assert!((stepped - route.distance_m).abs() < 1.0);
    }

    /// Walking uses paths a car may not, so the same pair routes differently — the
    /// point of having modes at all.
    #[test]
    fn modes_differ_and_all_three_find_a_route() {
        let g = graph();
        let a = tile_point(0.35, 0.4);
        let b = tile_point(0.6, 0.55);
        let mut seen = Vec::new();
        for mode in TravelMode::ALL {
            let r = g
                .route(a, b, mode)
                .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
            seen.push((mode, r.distance_m, r.duration_s));
        }
        let car = seen.iter().find(|(m, ..)| *m == TravelMode::Car).unwrap();
        let foot = seen.iter().find(|(m, ..)| *m == TravelMode::Foot).unwrap();
        assert!(foot.2 > car.2, "walking must be slower than driving");
        assert!(
            foot.1 <= car.1 * 1.05,
            "walking may cut through where a car may not: foot {:.0} m vs car {:.0} m",
            foot.1,
            car.1
        );
    }

    /// A car must never be routed onto a footway or a cycleway, and a bike must
    /// never be sent down a motorway. This is the access filter, checked at the
    /// table rather than through a route that might happen to avoid them anyway.
    #[test]
    fn access_filter_matches_the_mode() {
        for kind in [
            RoadKind::Footway,
            RoadKind::Cycleway,
            RoadKind::Steps,
            RoadKind::Pedestrian,
        ] {
            assert!(
                kind.speed_kmh(TravelMode::Car).is_none(),
                "{kind:?} open to cars"
            );
        }
        for kind in [RoadKind::Motorway, RoadKind::Trunk] {
            assert!(
                kind.speed_kmh(TravelMode::Bike).is_none(),
                "{kind:?} open to bikes"
            );
            assert!(
                kind.speed_kmh(TravelMode::Foot).is_none(),
                "{kind:?} open to walkers"
            );
        }
        assert!(RoadKind::Rail.speed_kmh(TravelMode::Foot).is_none());
        // …and a motorway is the fastest thing a car has.
        let car_speeds: Vec<f64> = [
            RoadKind::Motorway,
            RoadKind::Residential,
            RoadKind::LivingStreet,
        ]
        .iter()
        .map(|k| k.speed_kmh(TravelMode::Car).unwrap())
        .collect();
        assert!(car_speeds[0] > car_speeds[1] && car_speeds[1] > car_speeds[2]);
    }

    /// One-ways bind cars and not pedestrians — the asymmetry that keeps a walking
    /// route from taking an absurd loop around a city-centre one-way system.
    #[test]
    fn oneway_binds_only_the_modes_that_respect_it() {
        assert!(TravelMode::Car.respects_oneway());
        assert!(!TravelMode::Bike.respects_oneway());
        assert!(!TravelMode::Foot.respects_oneway());

        let g = graph();
        let edge = Edge {
            to: 0,
            len_m: 100.0,
            kind: RoadKind::Residential,
            against_oneway: true,
            bike_denied: false,
            unpaved: false,
            level: 0,
        };
        assert!(g.edge_cost(&edge, TravelMode::Car).is_none());
        assert!(g.edge_cost(&edge, TravelMode::Foot).is_some());
    }

    #[test]
    fn steps_carry_street_names_from_the_label_layer() {
        let g = graph();
        let route = g
            .route(
                tile_point(0.3, 0.35),
                tile_point(0.7, 0.65),
                TravelMode::Car,
            )
            .expect("route");
        let named = route
            .steps
            .iter()
            .filter(|s| {
                s.name
                    .as_deref()
                    .is_some_and(|n| !n.starts_with("the ") && !n.is_empty())
            })
            .count();
        assert!(
            named > 0,
            "no step picked up a name from `street_labels`: {:?}",
            route.steps.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    /// The routable core must be strongly connected: from any node in it, every
    /// other core node is reachable. This is the property `route` leans on when it
    /// snaps both endpoints into the core — both in one SCC ⇒ a path exists — and it
    /// is what stops a one-way stub near an address from trapping the search (the
    /// car failure where a two-way bike route worked but the car got `NoPath`). Also
    /// check the core actually excludes something, or it would not be doing its job.
    #[test]
    fn routable_core_is_strongly_connected() {
        let g = graph();
        for mode in TravelMode::ALL {
            let core = g.routable_core(mode);
            let members: Vec<u32> = (0..core.len() as u32)
                .filter(|&i| core[i as usize])
                .collect();
            assert!(
                members.len() > 100,
                "{mode:?}: core is only {} nodes",
                members.len()
            );

            // From an arbitrary core node, the forward-reachable set covers the core.
            let reach: std::collections::HashSet<u32> =
                g.reachable(members[0], mode).into_iter().collect();
            for &m in &members {
                assert!(
                    reach.contains(&m),
                    "{mode:?}: core node {m} not reachable from the core"
                );
            }

            // The car network has one-way stubs and service loops the core drops;
            // if the core equalled every usable node it would not be filtering them.
            let usable = (0..core.len() as u32)
                .filter(|&i| {
                    g.edges[i as usize]
                        .iter()
                        .any(|e| g.edge_cost(e, mode).is_some())
                })
                .count();
            if mode == TravelMode::Car {
                assert!(
                    members.len() < usable,
                    "car core {} == usable {usable}: no stubs excluded?",
                    members.len()
                );
            }
        }
    }

    /// A start snapped to a one-way stub must still route: `route` snaps into the
    /// core, not onto the nearest node. Regression for a one-way-stub start that
    /// returned `NoPath` for cars while routing fine for bikes.
    #[test]
    fn a_stub_start_still_routes() {
        let g = graph();
        // Find a node whose forward-reachable car set is a tiny pocket — a one-way
        // stub — and confirm a car route starting on top of it still succeeds.
        let core = g.routable_core(TravelMode::Car);
        let stub = (0..g.coords.len() as u32).find(|&i| {
            g.edges[i as usize]
                .iter()
                .any(|e| g.edge_cost(e, TravelMode::Car).is_some())
                && !core[i as usize]
                && g.reachable(i, TravelMode::Car).len() < 100
        });
        let Some(stub) = stub else {
            // No such pocket in this fixture — nothing to regress against, and the
            // strongly-connected-core test already covers the mechanism.
            return;
        };
        let (slat, slng) = g.node_lat_lng(stub);
        let route = g
            .route((slat, slng), tile_point(0.7, 0.65), TravelMode::Car)
            .expect("a stub start must snap into the core and route");
        assert!(route.distance_m > 0.0 && !route.steps.is_empty());
    }

    /// The noding pass is load-bearing, and its absence does not announce itself —
    /// it just returns `NoPath` for ordinary routes. Pin both halves of the fix:
    /// every tee it was built for is a real vertex afterwards, and the network is
    /// one piece.
    ///
    /// "Every tee", not "every contact". The pass deliberately leaves nodes lying on
    /// segments now — a street passing under a viaduct is exactly that, and joining
    /// the two is the bug the guards exist to prevent. What must not be left lying
    /// is a **dangle at a shared grade**: a line that stops dead on another line it
    /// could really have met. That is the invariant, and stating it this way is what
    /// keeps the test from being satisfied by a pass that simply joins less.
    #[test]
    fn noding_reconnects_the_network() {
        let g = graph();

        let mut segs = Vec::new();
        for (i, adj) in g.edges.iter().enumerate() {
            for e in adj {
                if e.to as usize > i {
                    segs.push((i, e.to as usize));
                }
            }
        }
        let dangling = (0..g.coords.len())
            .filter(|&i| g.node_terminal[i])
            .filter(|&i| {
                segs.iter().any(|&(a, b)| {
                    a != i
                        && b != i
                        // A grade all three share, or they were never in contact.
                        && g.node_levels[i] & g.node_levels[a] & g.node_levels[b] != 0
                        && point_seg_distance(g.coords[i], g.coords[a], g.coords[b])
                            <= NODE_TOL
                })
            })
            .count();
        assert!(
            dangling <= g.coords.len() / 100,
            "{dangling} dangling ends still sit on a segment they could have joined"
        );

        // …and the usable network is essentially one component per mode. Before
        // noding this was 58% for cars, which is what made a cross-town route fail.
        for (mode, floor) in [
            (TravelMode::Car, 0.85),
            (TravelMode::Bike, 0.90),
            (TravelMode::Foot, 0.90),
        ] {
            let (largest, usable) = g.largest_component(mode);
            assert!(
                largest as f64 >= usable as f64 * floor,
                "{mode:?}: largest component {largest} of {usable} usable nodes"
            );
        }
    }

    #[test]
    fn turn_classification() {
        assert_eq!(Maneuver::from_turn(0.0), Maneuver::Continue);
        assert_eq!(Maneuver::from_turn(-90.0), Maneuver::Left);
        assert_eq!(Maneuver::from_turn(90.0), Maneuver::Right);
        assert_eq!(Maneuver::from_turn(-25.0), Maneuver::SlightLeft);
        assert_eq!(Maneuver::from_turn(175.0), Maneuver::UTurn);
        assert_eq!(Maneuver::from_turn(-170.0), Maneuver::UTurn);
        // Bearings wrap: north-ish to north-ish across 0° is a small turn, not the
        // 340° one a naive subtraction would report.
        assert_eq!(signed_turn(350.0, 5.0), 15.0);
        assert_eq!(
            Maneuver::from_turn(signed_turn(350.0, 5.0)),
            Maneuver::Continue
        );
    }

    #[test]
    fn a_missing_area_fails_with_a_reason_not_a_panic() {
        let g = graph();
        // Mid-Atlantic: nothing to snap onto.
        assert_eq!(
            g.route((0.0, -30.0), (0.1, -30.1), TravelMode::Car).err(),
            Some(RouteError::NoStart)
        );
        assert_eq!(
            RoadGraph::new(0.0)
                .route((0.0, 0.0), (1.0, 1.0), TravelMode::Car)
                .err(),
            Some(RouteError::NoData)
        );
    }

    /// The corridor is a capsule, not a bounding box: a diagonal route must cost
    /// far fewer tiles than the square it spans.
    #[test]
    fn corridor_is_narrower_than_a_bounding_box() {
        let a = (53.16, 6.45);
        let b = (53.26, 6.66);
        let tiles = corridor_tiles(a, b, MAX_ROUTE_TILES).expect("within budget");
        let ta = lat_lng_to_tile_frac(a.0, a.1, ROUTING_ZOOM);
        let tb = lat_lng_to_tile_frac(b.0, b.1, ROUTING_ZOOM);
        let bbox =
            ((ta.0 - tb.0).abs().floor() as usize + 3) * ((ta.1 - tb.1).abs().floor() as usize + 3);
        assert!(
            tiles.len() < bbox,
            "corridor {} tiles vs bbox {bbox}",
            tiles.len()
        );
        // Both endpoints' own tiles must be in it, or nothing can snap.
        for p in [ta, tb] {
            assert!(tiles.contains(&(p.0.floor() as i64, p.1.floor() as i64)));
        }
        // Nearest-first ordering, so a partial fetch is still useful.
        assert_eq!(tiles[0], (ta.0.floor() as i64, ta.1.floor() as i64));
    }

    /// The capsule's width must grow sub-linearly, so a long route stays thin — the
    /// regression for a cross-province query that once asked for hundreds of tiles.
    /// A route four times as long must not cost four times the *width*.
    #[test]
    fn the_corridor_stays_thin_as_it_lengthens() {
        // Two mostly-east–west spans from a common origin, one ~4× the other, both
        // within the tile budget.
        let origin = (53.20, 6.00);
        let short = corridor_tiles(origin, (53.20, 6.05), MAX_ROUTE_TILES).unwrap();
        let long = corridor_tiles(origin, (53.20, 6.20), MAX_ROUTE_TILES).unwrap();
        // Width ≈ tiles per unit of length. The long route is ~4× longer; if width
        // grew with length its tiles-per-length would climb, and it must not.
        let span = |ts: &[(i64, i64)]| {
            let xs: Vec<i64> = ts.iter().map(|t| t.0).collect();
            (xs.iter().max().unwrap() - xs.iter().min().unwrap()).max(1) as f64
        };
        let density = |ts: &[(i64, i64)]| ts.len() as f64 / span(ts);
        assert!(
            density(&long) <= density(&short) * 1.2,
            "long corridor is wider per km: {:.1} vs {:.1} tiles/tile-of-length",
            density(&long),
            density(&short)
        );
    }

    #[test]
    fn an_overlong_route_is_refused_with_a_distance() {
        // ~150 km apart — far past what a single-zoom corridor can hold.
        let err = corridor_tiles((52.09, 5.11), (53.22, 6.57), MAX_ROUTE_TILES).unwrap_err();
        let RouteError::TooFar { km } = err else {
            panic!("expected TooFar, got {err:?}");
        };
        assert!(
            (100.0..250.0).contains(&km),
            "implausible distance {km:.0} km"
        );
        // The message leads with the distance, not a tile count.
        let msg = err.to_string();
        assert!(msg.contains("Too far") && msg.contains("km"), "{msg}");
    }

    #[test]
    fn formatting() {
        assert_eq!(format_distance(432.0), "430 m");
        assert_eq!(format_distance(4321.0), "4.3 km");
        assert_eq!(format_distance(43_210.0), "43 km");
        assert_eq!(format_duration(30.0), "1 min");
        assert_eq!(format_duration(510.0), "9 min");
        assert_eq!(format_duration(4_320.0), "1 h 12 min");
    }
}
