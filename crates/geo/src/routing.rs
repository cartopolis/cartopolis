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

/// How the route is travelled. The three land modes a map app offers; each one is
/// a different access filter and speed table over the same graph.
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
    /// [`Self::route`] snaps into the [routable core](Self::routable_core) instead,
    /// and asks this one only to tell its two failures apart: a point with no
    /// usable road near it at all is "no road there", while a point with one that
    /// merely isn't in the same component is a genuine `NoPath`.
    #[must_use]
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

/// "450 m" / "4.3 km" — the phone-map distance style: metres under a kilometre,
/// then one decimal.
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
mod tests;
