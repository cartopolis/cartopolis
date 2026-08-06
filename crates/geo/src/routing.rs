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

use std::collections::{BinaryHeap, HashMap};

use geo_types::Geometry;
use mvt_reader::feature::{Feature, Value};

use viberfox_core::world::tile_to_meters;

use viberfox_core::lat_lng_to_tile_frac;
use crate::vector_tiles::{TileData, MAX_SOURCE_ZOOM};

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

    #[must_use]
    pub fn icon(self) -> &'static str {
        match self {
            Self::Car => "🚗",
            Self::Bike => "🚲",
            Self::Foot => "🚶",
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
                // No cars on foot/cycle ways, in bus lanes, or on rails.
                Self::Pedestrian
                | Self::Footway
                | Self::Steps
                | Self::Path
                | Self::Cycleway
                | Self::Busway
                | Self::Rail
                | Self::Other => return None,
            }),
            TravelMode::Bike => Some(match self {
                // Motorways and trunk roads are closed to bikes.
                Self::Motorway | Self::Trunk | Self::Rail | Self::Busway => return None,
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
}

impl RoadGraph {
    /// An empty graph scaled for routes around `center_lat`.
    #[must_use]
    pub fn new(center_lat: f64) -> Self {
        Self {
            index: HashMap::new(),
            coords: Vec::new(),
            raw: Vec::new(),
            edges: Vec::new(),
            built: false,
            node_buckets: HashMap::new(),
            names: Vec::new(),
            label_segs: Vec::new(),
            label_buckets: HashMap::new(),
            m_per_unit: tile_to_meters(ROUTING_ZOOM, center_lat) / EXTENT,
            zoom_n: 2.0_f64.powi(ROUTING_ZOOM as i32),
        }
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
            let routable = match meta.name.as_str() {
                "streets" => true,
                "street_labels" => false,
                _ => continue,
            };
            let features = tile_data.features(meta);
            // Normalise a source that ships a different extent onto our lattice,
            // so tiles from mixed sources still stitch (see the module docs).
            let extent = f64::from(TileData::extent(meta));
            let scale = EXTENT / extent;
            let ox = tile.0 as f64 * EXTENT;
            let oy = tile.1 as f64 * EXTENT;
            let to_global =
                |c: &geo_types::Coord<f32>| (ox + f64::from(c.x) * scale, oy + f64::from(c.y) * scale);

            for feature in features {
                if routable {
                    self.add_street(feature, &to_global);
                } else {
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
        for seg in &raw {
            let (a, b) = (self.coords[seg.a as usize], self.coords[seg.b as usize]);
            splits.clear();

            // Candidate splitters: every node in the buckets the segment's bounding
            // box touches. Segments are short (a city block at most), so this is a
            // handful of buckets each.
            let (x0, x1) = minmax(a.0, b.0);
            let (y0, y1) = minmax(a.1, b.1);
            for bx in bucket(x0 - NODE_TOL)..=bucket(x1 + NODE_TOL) {
                for by in bucket(y0 - NODE_TOL)..=bucket(y1 + NODE_TOL) {
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
                        if d <= NODE_TOL && t > 1e-6 && t < 1.0 - 1e-6 {
                            splits.push((t, id));
                        }
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
        });
        self.edges[b as usize].push(Edge {
            to: a,
            len_m,
            kind: seg.kind,
            against_oneway: seg.rev_blocked,
            bike_denied: seg.bike_denied,
            unpaved: seg.unpaved,
        });
    }

    fn add_street(
        &mut self,
        feature: &Feature,
        to_global: &impl Fn(&geo_types::Coord<f32>) -> (f64, f64),
    ) {
        let props = feature.properties.as_ref();
        let kind = RoadKind::parse(str_prop(props, "kind").unwrap_or(""));
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
            Some("unpaved" | "gravel" | "ground" | "dirt" | "sand" | "grass" | "compacted" | "cobblestone")
        );

        // `oneway_reverse` says the legal direction runs *against* the digitised
        // geometry, so it swaps which traversal is barred.
        let (fwd_blocked, rev_blocked) = match (oneway, reversed) {
            (false, _) => (false, false),
            (true, false) => (false, true),
            (true, true) => (true, false),
        };
        let mut add_line = |line: &geo_types::LineString<f32>| {
            let pts: Vec<(f64, f64)> = line.coords().map(to_global).collect();
            for w in pts.windows(2) {
                let (ia, ib) = (self.node(w[0]), self.node(w[1]));
                if ia == ib {
                    continue; // zero-length after quantisation
                }
                self.raw.push(RawSeg {
                    a: ia,
                    b: ib,
                    kind,
                    fwd_blocked,
                    rev_blocked,
                    bike_denied,
                    unpaved,
                });
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
        to_global: &impl Fn(&geo_types::Coord<f32>) -> (f64, f64),
    ) {
        let props = feature.properties.as_ref();
        let Some(name) = str_prop(props, "name").filter(|n| !n.is_empty()) else {
            return;
        };
        let name_id = self.names.len() as u32;
        self.names.push(name.to_string());

        let mut any = false;
        let mut add_line = |line: &geo_types::LineString<f32>| {
            let pts: Vec<(f64, f64)> = line.coords().map(to_global).collect();
            for w in pts.windows(2) {
                let id = self.label_segs.len() as u32;
                self.label_segs.push(LabelSeg {
                    a: w[0],
                    b: w[1],
                    name: name_id,
                });
                // Register in every bucket the segment's bounding box touches, so a
                // 3×3 bucket probe around a point always sees a nearby segment.
                let (x0, x1) = minmax(w[0].0, w[1].0);
                let (y0, y1) = minmax(w[0].1, w[1].1);
                for bx in bucket(x0)..=bucket(x1) {
                    for by in bucket(y0)..=bucket(y1) {
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

    /// Intern a lattice point as a node id.
    fn node(&mut self, p: (f64, f64)) -> u32 {
        let key: Node = (p.0.round() as i32, p.1.round() as i32);
        if let Some(&id) = self.index.get(&key) {
            return id;
        }
        let id = self.coords.len() as u32;
        self.index.insert(key, id);
        self.coords.push((f64::from(key.0), f64::from(key.1)));
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
        let (tx, ty) = lat_lng_to_tile_frac(lat, lng, ROUTING_ZOOM);
        (tx * EXTENT, ty * EXTENT)
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
        let p = Self::lat_lng_to_lattice(lat, lng);
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
        best.filter(|(d2, _)| d2.sqrt() <= max_units).map(|(_, id)| id)
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
            .map(|i| self.edges[i].iter().any(|e| self.edge_cost(e, mode).is_some()))
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
        let start = self
            .nearest_node_where(from.0, from.1, SNAP_M, in_core)
            .ok_or(RouteError::NoStart)?;
        let goal = self
            .nearest_node_where(to.0, to.1, SNAP_M, in_core)
            .ok_or(RouteError::NoEnd)?;
        if start == goal {
            return Err(RouteError::TooClose);
        }

        let path = self.astar(start, goal, mode).ok_or(RouteError::NoPath)?;
        Ok(self.build_route(&path, mode, from, to))
    }

    /// A* over travel time. The heuristic is the straight-line distance divided by
    /// the mode's top speed — admissible, since no edge can beat that.
    fn astar(&self, start: u32, goal: u32, mode: TravelMode) -> Option<Vec<u32>> {
        let goal_c = self.coords[goal as usize];
        let h = |id: u32| -> f64 {
            let c = self.coords[id as usize];
            ((c.0 - goal_c.0).powi(2) + (c.1 - goal_c.1).powi(2)).sqrt() * self.m_per_unit
                / mode.max_speed_ms()
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
                duration_s: self.edge_cost(edge, mode).unwrap_or(0.0),
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

    /// Arrow glyphs for the turn list, best first.
    ///
    /// A list rather than one glyph because the panel's font may not have the nice
    /// one — `navigation::first_drawable` picks the first it can actually draw, and
    /// the last entry is ASCII so it always can. A terminal (the `solves_a_real_route`
    /// check) just takes the head.
    #[must_use]
    pub fn icon_candidates(self) -> &'static [&'static str] {
        match self {
            Self::Depart => &["⬤", "●", "•", "o"],
            Self::Continue => &["⬆", "↑", "^"],
            Self::SlightLeft => &["↖", "⬉", "<"],
            Self::Left | Self::SharpLeft => &["↰", "⬅", "←", "<"],
            Self::SlightRight => &["↗", "⬈", ">"],
            Self::Right | Self::SharpRight => &["↱", "➡", "→", ">"],
            Self::UTurn => &["↻", "↩", "U"],
            Self::Arrive => &["⚑", "🏁", "◆", "x"],
        }
    }
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
                "Too far to route here — {km:.0} km is beyond the ~{MAX_ROUTE_KM:.0} km \
                 reach of street-level routing"
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
    let a = lat_lng_to_tile_frac(from.0, from.1, ROUTING_ZOOM);
    let b = lat_lng_to_tile_frac(to.0, to.1, ROUTING_ZOOM);
    let direct = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let half_width =
        (CORRIDOR_BASE + CORRIDOR_SPREAD * direct.sqrt()).min(CORRIDOR_MAX_HALF_WIDTH);

    let (x0, x1) = minmax(a.0, b.0);
    let (y0, y1) = minmax(a.1, b.1);
    let pad = (half_width.max(ENDPOINT_RING) + 1.0).ceil() as i64;
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
        let km = direct * tile_to_meters(ROUTING_ZOOM, mid_lat) / 1000.0;
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

// ─── Formatting helpers (shared by the panel and the tests) ──────────────────

/// "450 m" / "4.3 km" — Organic Maps' distance style.
#[must_use]
pub fn format_distance(m: f64) -> String {
    if m < 1000.0 {
        format!("{:.0} m", (m / 10.0).round() * 10.0)
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

fn str_prop<'a>(props: Option<&'a HashMap<String, Value>>, key: &str) -> Option<&'a str> {
    match props?.get(key)? {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}

fn bool_prop(props: Option<&HashMap<String, Value>>, key: &str) -> bool {
    matches!(props.and_then(|p| p.get(key)), Some(Value::Bool(true)))
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

    /// The same real z14 Groningen tile the renderer tests use.
    const GRONINGEN_Z14: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/groningen_z14.mvt"));
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
            let r = g.route(a, b, mode).unwrap_or_else(|e| panic!("{mode:?}: {e}"));
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
        for kind in [RoadKind::Footway, RoadKind::Cycleway, RoadKind::Steps, RoadKind::Pedestrian] {
            assert!(kind.speed_kmh(TravelMode::Car).is_none(), "{kind:?} open to cars");
        }
        for kind in [RoadKind::Motorway, RoadKind::Trunk] {
            assert!(kind.speed_kmh(TravelMode::Bike).is_none(), "{kind:?} open to bikes");
            assert!(kind.speed_kmh(TravelMode::Foot).is_none(), "{kind:?} open to walkers");
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
        };
        assert!(g.edge_cost(&edge, TravelMode::Car).is_none());
        assert!(g.edge_cost(&edge, TravelMode::Foot).is_some());
    }

    #[test]
    fn steps_carry_street_names_from_the_label_layer() {
        let g = graph();
        let route = g
            .route(tile_point(0.3, 0.35), tile_point(0.7, 0.65), TravelMode::Car)
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
            let members: Vec<u32> = (0..core.len() as u32).filter(|&i| core[i as usize]).collect();
            assert!(members.len() > 100, "{mode:?}: core is only {} nodes", members.len());

            // From an arbitrary core node, the forward-reachable set covers the core.
            let reach: std::collections::HashSet<u32> =
                g.reachable(members[0], mode).into_iter().collect();
            for &m in &members {
                assert!(reach.contains(&m), "{mode:?}: core node {m} not reachable from the core");
            }

            // The car network has one-way stubs and service loops the core drops;
            // if the core equalled every usable node it would not be filtering them.
            let usable = (0..core.len() as u32)
                .filter(|&i| g.edges[i as usize].iter().any(|e| g.edge_cost(e, mode).is_some()))
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
    /// nothing is left lying on a segment afterwards, and the network is one piece.
    #[test]
    fn noding_reconnects_the_network() {
        let g = graph();

        // Every tee has become a real vertex.
        let mut segs = Vec::new();
        for (i, adj) in g.edges.iter().enumerate() {
            for e in adj {
                if e.to as usize > i {
                    segs.push((g.coords[i], g.coords[e.to as usize]));
                }
            }
        }
        let dangling = g
            .coords
            .iter()
            .filter(|c| {
                segs.iter().any(|(a, b)| {
                    *a != **c && *b != **c && point_seg_distance(**c, *a, *b) <= NODE_TOL
                })
            })
            .count();
        assert!(
            dangling <= g.coords.len() / 100,
            "{dangling} nodes still sit on a segment they are not a vertex of"
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
        assert_eq!(Maneuver::from_turn(signed_turn(350.0, 5.0)), Maneuver::Continue);
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
        let bbox = ((ta.0 - tb.0).abs().floor() as usize + 3)
            * ((ta.1 - tb.1).abs().floor() as usize + 3);
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
        assert!((100.0..250.0).contains(&km), "implausible distance {km:.0} km");
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
