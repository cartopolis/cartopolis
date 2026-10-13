//! What the BAG says about a building we are already drawing.
//!
//! The 3DBAG lanes supply a building's shape; this supplies its *facts* — year
//! built, what it is used for, how big it is, whether the register still thinks
//! it exists — plus the addresses and the houseboat berths in the same cell.
//! Served from our own host by atlas's `pipeline/bag.rs` as
//! `bag/{14}/{x}/{y}.json`, alongside the 3DBAG trees and behind the same
//! coverage manifest pattern.
//!
//! **The join is an id, not a geometry test.** atlas's `pipeline/lod22.rs` writes each
//! mesh element's BAG *identificatie* verbatim, and every record here is keyed on
//! the same string, so a building either carries the key or it does not — there is
//! no tolerance to tune and no nearest-match to be wrong about. That id is also
//! the join to everything else in the Dutch registers (addresses, monuments, energy
//! labels) and to any BIM model of the building, which is the reason it is carried
//! at all rather than being an internal dedupe number.
//!
//! **Two consumers, two shapes, and that is deliberate.** A colour baked into a
//! vertex has to be known on the worker that builds the mesh, so `lod22` fetches
//! the cell's facts *inside its own job* through [`fetch_bag_text`] — a second
//! request that is a cache hit, and in exchange the mesh and the facts it is
//! coloured by can never arrive in the wrong order. Everything else (a readout, a
//! lookup, a future panel) wants a resource it can ask at any time, which is what
//! [`BuildingFacts`] and the streamer below are for. Making the mesh wait for the
//! resource would mean rebuilding meshes whenever a facts cell landed, which is
//! the terrain-revision retear — affordable for a 6.4 km field that arrives once,
//! not for a stream. What the two *do* share is the parse ([`parse_part`]):
//! whichever asks first parses a file, and the other is handed the same one.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;

use crate::grid::AnchoredCamera;
use crate::platform::stream::{CellStream, Policy};
use crate::platform::{http, storage};
use crate::systems::cell_ring;
use crate::systems::coverage::CoverageSlot;
use crate::systems::globe::{GlobeState, GLOBE_SHOW_ALT};
use crate::systems::map_stream::MapStream;
use crate::systems::osm_buildings::tdbag_url;
use cartopolis_core::world::{lat_lng_to_tile, tile_to_meters};

/// Fixed cache/streaming zoom — the one the extractor bins into.
const BAG_ZOOM: u32 = cartopolis_geo::formats::CACHE_ZOOM;

/// Cells streamed around the camera. Matches the LoD2.2 ring: these facts exist to
/// describe buildings you can see, and a cell is ~200 kB against that lane's ~4 MB.
const BAG_RADIUS: i64 = 2;

/// Concurrent cell fetches. Our own host, so this is throughput, not courtesy.
const BAG_CONCURRENCY: usize = 4;
/// Attempts per cell before it rests (`platform::stream` policy).
const BAG_MAX_ATTEMPTS: u32 = 3;
const BAG_BACKOFF_SECS: f64 = 2.0;
const BAG_BACKOFF_DOUBLINGS: u32 = 2;
const BAG_RETRY_AFTER_SECS: f64 = 60.0;

// ─── The BAG's `gebruiksdoel`, as a bitmask ──────────────────────────────────

/// A building's use, as the bitmask the wire carries.
///
/// A pand routinely has several — a shop with flats above it is the ordinary
/// Dutch street — so this is a set, not an enum. The bit order is the wire format
/// and is mirrored by `USE_BITS` in atlas's `pipeline/bag.rs`: **append, never
/// renumber**.
pub mod uses {
    pub const HOME: u16 = 1 << 0;
    pub const SHOP: u16 = 1 << 1;
    pub const OFFICE: u16 = 1 << 2;
    pub const MEETING: u16 = 1 << 3;
    pub const INDUSTRY: u16 = 1 << 4;
    pub const EDUCATION: u16 = 1 << 5;
    pub const HEALTHCARE: u16 = 1 << 6;
    pub const LODGING: u16 = 1 << 7;
    pub const SPORT: u16 = 1 << 8;
    pub const CELL: u16 = 1 << 9;
    pub const OTHER: u16 = 1 << 10;
}

/// An English label for the lowest set bit, for a readout. `None` for an empty
/// mask — the register saying nothing is not the same as it saying "other".
pub fn use_label(mask: u16) -> Option<&'static str> {
    const NAMES: [(u16, &str); 11] = [
        (uses::HOME, "home"),
        (uses::SHOP, "shop"),
        (uses::OFFICE, "office"),
        (uses::MEETING, "meeting place"),
        (uses::INDUSTRY, "industry"),
        (uses::EDUCATION, "education"),
        (uses::HEALTHCARE, "healthcare"),
        (uses::LODGING, "lodging"),
        (uses::SPORT, "sport"),
        (uses::CELL, "detention"),
        (uses::OTHER, "other"),
    ];
    NAMES
        .iter()
        .find(|(bit, _)| mask & bit != 0)
        .map(|(_, name)| *name)
}

/// **Every** use in the mask, in bit order.
///
/// [`use_label`] answers with the first one because a one-line readout has room
/// for one word. A shop with flats over it is the ordinary Dutch street, so a
/// panel that has room to say so should say so — dropping the rest there would be
/// a register fact quietly thrown away in the one place somebody went looking for
/// it.
pub fn use_labels(mask: u16) -> Vec<&'static str> {
    const NAMES: [(u16, &str); 11] = [
        (uses::HOME, "homes"),
        (uses::SHOP, "shops"),
        (uses::OFFICE, "offices"),
        (uses::MEETING, "meeting place"),
        (uses::INDUSTRY, "industry"),
        (uses::EDUCATION, "education"),
        (uses::HEALTHCARE, "healthcare"),
        (uses::LODGING, "lodging"),
        (uses::SPORT, "sport"),
        (uses::CELL, "detention"),
        (uses::OTHER, "other"),
    ];
    NAMES
        .iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// What the register says about a building's existence.
///
/// Almost every building is `InUse`, which is why the wire omits the field for it.
/// The rest matter because 3DBAG's geometry and the BAG's status are refreshed on
/// different cadences: a building can be drawn from a solid the register has since
/// marked demolished, or not built yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BuildingStatus {
    #[default]
    InUse,
    InUseUnmeasured,
    Permit,
    UnderConstruction,
    Renovation,
    OutOfUse,
    DemolitionPermit,
    Demolished,
    NotRealised,
    Withdrawn,
    /// A value the register grew after this was written. Kept distinct from
    /// `InUse` so it cannot be mistaken for a positive answer.
    Unknown,
}

impl BuildingStatus {
    fn parse(s: &str) -> Self {
        match s {
            "in_use_unmeasured" => Self::InUseUnmeasured,
            "permit" => Self::Permit,
            "building" => Self::UnderConstruction,
            "renovation" => Self::Renovation,
            "out_of_use" => Self::OutOfUse,
            "demolition_permit" => Self::DemolitionPermit,
            "demolished" => Self::Demolished,
            "not_realised" => Self::NotRealised,
            "withdrawn" => Self::Withdrawn,
            _ => Self::Unknown,
        }
    }

    /// Whether the register believes the building is standing and usable. False
    /// for a demolished or never-built pand — the cue a consumer wants when it is
    /// deciding whether to light the windows or draw it at all.
    pub fn standing(self) -> bool {
        matches!(self, Self::InUse | Self::InUseUnmeasured | Self::Renovation)
    }

    /// The register's own answer, in English, for a readout.
    ///
    /// `None` for the two ordinary in-use values: a card that says "status: in
    /// use" about nineteen buildings in twenty has spent a row on nothing. The
    /// rest are all worth reading — a building under construction or awaiting
    /// demolition is a fact about the street you are standing in.
    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::InUse | Self::InUseUnmeasured => None,
            Self::Permit => Some("permit granted"),
            Self::UnderConstruction => Some("under construction"),
            Self::Renovation => Some("being renovated"),
            Self::OutOfUse => Some("out of use"),
            Self::DemolitionPermit => Some("demolition permit"),
            Self::Demolished => Some("demolished"),
            Self::NotRealised => Some("never built"),
            Self::Withdrawn => Some("registration withdrawn"),
            Self::Unknown => Some("unknown"),
        }
    }
}

// ─── The facts themselves ────────────────────────────────────────────────────

/// One building, as the BAG describes it.
#[derive(Clone, Debug, Default)]
pub struct BuildingFact {
    /// `oorspronkelijkBouwjaar`. `None` when the register has no year, which is
    /// rare but real — and distinct from a year of 0, which some sources emit.
    pub year: Option<u16>,
    /// [`uses`] bitmask; 0 when the register says nothing.
    pub uses: u16,
    /// Floor area of the smallest unit inside, m².
    pub area_m2: Option<u32>,
    /// Floor area of the largest unit, m², when it differs from `area_m2`.
    pub area_max_m2: Option<u32>,
    /// Addressable units inside (0 = none, e.g. a shed or a garage block).
    pub units: u16,
    pub status: BuildingStatus,
    /// National monument number (`rijksmonument`), if the heritage register has
    /// one for this building. The number is the register's own and resolves at
    /// `monumentenregister.cultureelerfgoed.nl/monumenten/<n>`.
    pub monument: Option<u32>,
    /// The monument's broad category — "Religieuze gebouwen", "Woningen en
    /// woningbouwcomplexen". Dutch, because it is the register's own word and
    /// translating a controlled vocabulary invents distinctions it does not make.
    pub monument_category: Option<Arc<str>>,
    /// The protected historic district (*beschermd stadsgezicht*) this building
    /// stands in, if any. Interned per cell — a district covers thousands of
    /// buildings and its name is one string.
    pub district: Option<Arc<str>>,
    /// The building's own outline, as lat/lng, open (no repeated first point).
    ///
    /// The shape the client never had. A 3DBAG building arrives as a triangle
    /// soup, from which the only footprint recoverable is the **convex hull** —
    /// right for a rectangle, wrong for every L-shaped building, whose inner
    /// corner it fills in with a collider nobody can walk through. This is the
    /// register's own ring.
    pub footprint: Vec<(f64, f64)>,
    /// Where this building's addressed units are entered, as lat/lng — a point
    /// **on** the facade, one per unit.
    ///
    /// Derived, not surveyed. The register's own point for a unit is somewhere
    /// inside it, and nobody publishes door positions at all, so
    /// [`place_doors_on_facades`] moves each one out to a wall as the cell is
    /// parsed. Every consumer — the drawn panel in `lod22`, the gap
    /// `collision::Footprint` opens, and the doorway `interiors` cuts out of the
    /// room's own wall — wants that point and not the raw one.
    pub doors: Vec<(f64, f64)>,
}

/// One addressable unit — the BAG's `verblijfsobject`.
///
/// `at` is the register's own point for the unit, which sits at or near its
/// entrance. That is the closest thing to a door position anybody has for the
/// whole country, and it is why this is carried rather than only the pand row.
#[derive(Clone, Debug)]
pub struct AddressFact {
    pub at: (f64, f64),
    /// The pand this unit is in, when the register links one.
    pub pand: Option<Arc<str>>,
    pub street: Option<Arc<str>>,
    pub number: Option<String>,
    pub postcode: Option<String>,
    pub uses: u16,
    pub area_m2: Option<u32>,
}

impl AddressFact {
    /// `"Paterswoldseweg 62"`, or `None` if the register gave neither half.
    pub fn label(&self) -> Option<String> {
        match (self.street.as_deref(), self.number.as_deref()) {
            (Some(s), Some(n)) => Some(format!("{s} {n}")),
            (Some(s), None) => Some(s.to_string()),
            (None, Some(n)) => Some(n.to_string()),
            (None, None) => None,
        }
    }
}

/// A berth for a houseboat (`ligplaats`) or a plot for a mobile home
/// (`standplaats`) — an addressed place with no building on it, and therefore
/// invisible to every map drawn from buildings alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BerthKind {
    Mooring,
    Plot,
}

#[derive(Clone, Debug)]
pub struct BerthFact {
    pub kind: BerthKind,
    pub at: (f64, f64),
    pub street: Option<Arc<str>>,
    pub number: Option<String>,
    pub postcode: Option<String>,
}

/// One parsed cell.
#[derive(Clone, Debug, Default)]
pub struct BagCell {
    pub buildings: HashMap<Arc<str>, BuildingFact>,
    pub addresses: Vec<AddressFact>,
    pub berths: Vec<BerthFact>,
}

impl BagCell {
    /// Facts for a building id, if this cell knows it.
    pub fn building(&self, id: &str) -> Option<&BuildingFact> {
        self.buildings.get(id)
    }
}

// ─── Parsing ─────────────────────────────────────────────────────────────────

/// Parse a cell file. Typed rather than a `serde_json::Value` walk, for the reason
/// `lod22::parse_lod22` documents — this is smaller, but it runs on the same
/// worker and there is no reason to pay the DOM for it.
pub fn parse_bag_cell(text: &str) -> Result<BagCell, String> {
    // The file's own types, shared with the writer (atlas's `pipeline/bag.rs`).
    // Either of a cell's two files parses: each array the file lacks is `None`.
    let wire = cartopolis_geo::formats::bag::parse(text)?;
    // Intern once per cell: a street name is repeated by every unit on it, and
    // handing each `AddressFact` its own `String` would allocate thousands of
    // copies of "Paterswoldseweg" per cell.
    let streets: Vec<Arc<str>> = wire
        .streets
        .unwrap_or_default()
        .into_iter()
        .map(Arc::from)
        .collect();
    let street_of = |ix: Option<usize>| ix.and_then(|i| streets.get(i).cloned());
    let districts: Vec<Arc<str>> = wire
        .districts
        .unwrap_or_default()
        .into_iter()
        .map(Arc::from)
        .collect();
    let wire_buildings = wire.buildings.unwrap_or_default();

    // Monument categories are a closed vocabulary of a dozen phrases spread over
    // every listed building in the cell, so they are interned as they are seen
    // rather than shipped as a table: the table would have to be built by the
    // extractor for a saving the parser can make for itself.
    let mut categories: HashMap<String, Arc<str>> = HashMap::new();
    let mut buildings = HashMap::with_capacity(wire_buildings.len());
    for b in wire_buildings {
        buildings.insert(
            Arc::from(b.id.as_str()),
            BuildingFact {
                // A year of 0 is a placeholder, not a building from the year nought.
                year: b.year.filter(|y| *y > 0),
                uses: b.uses.unwrap_or(0),
                area_m2: b.area,
                area_max_m2: b.area_max,
                units: b.units.unwrap_or(0),
                status: b
                    .status
                    .as_deref()
                    .map_or(BuildingStatus::InUse, BuildingStatus::parse),
                monument: b.mon,
                monument_category: b.mcat.map(|c| {
                    categories
                        .entry(c)
                        .or_insert_with_key(|k| Arc::from(k.as_str()))
                        .clone()
                }),
                district: b.district.and_then(|i| districts.get(i).cloned()),
                // A ring under three points is not an outline; the extractor does
                // not write one and a truncated cache entry can.
                footprint: if b.fp.len() >= 3 { b.fp } else { Vec::new() },
                doors: b.doors,
            },
        );
    }
    let addresses = wire
        .addresses
        .unwrap_or_default()
        .into_iter()
        .map(|a| AddressFact {
            at: a.at,
            pand: a.id.map(|s| Arc::from(s.as_str())),
            street: street_of(a.street),
            number: a.nr,
            postcode: a.postcode,
            uses: a.uses.unwrap_or(0),
            area_m2: a.area,
        })
        .collect();
    let berths = wire
        .berths
        .unwrap_or_default()
        .into_iter()
        .map(|b| BerthFact {
            kind: if b.kind == cartopolis_geo::formats::bag::KIND_PLOT {
                BerthKind::Plot
            } else {
                BerthKind::Mooring
            },
            at: b.at,
            street: street_of(b.street),
            number: b.nr,
            postcode: b.postcode,
        })
        .collect();
    place_doors_on_facades(&mut buildings);
    Ok(BagCell {
        buildings,
        addresses,
        berths,
    })
}

// ─── Putting the register's point on a wall ──────────────────────────────────

/// How far off a wall the inside/outside probe is taken, metres. Well under a
/// wall's thickness and well over the precision of a ring rounded to 1 cm — the
/// same probe, for the same reason, as `lod22::nearest_edge_point`.
const PROBE_M: f64 = 0.05;

/// How far outside a wall the *neighbour* probe is taken, metres. Further out
/// than [`PROBE_M`]: two abutting pand rings are surveyed independently and
/// disagree by a few centimetres, so a probe that close to the wall answers the
/// rounding rather than the question.
const NEIGHBOUR_PROBE_M: f64 = 0.30;

/// How much of a wall a doorway needs either side of itself, metres. A little
/// over half the drawn panel (`lod22::DOOR_WIDTH_M`), so a door is never hung
/// across a corner and the wall it is on is unambiguously the nearest one.
const DOOR_CLEARANCE_M: f64 = 0.6;

/// A wall shorter than this cannot hold a door at all.
const MIN_DOOR_WALL_M: f64 = 1.2;

/// Move each register point onto the wall it is a doorway in.
///
/// **The BAG has no door positions and nothing else does either.** A
/// `verblijfsobject`'s point is the *unit's* — required only to lie inside the
/// pand — and over the Groningen centre cell it sits a median 1.8 m in from the
/// nearest wall. BGT has no door object, and OpenStreetMap's surveyed
/// `entrance` nodes number 137 for the whole of central Groningen against ~8,000
/// buildings. So a doorway has to be *derived*, and the only question is by what
/// rule.
///
/// The rule that was here — draw the panel on whichever wall is nearest — is the
/// obvious one and it fails on the ordinary Dutch terrace, where the wall
/// nearest a unit's point is frequently the **party wall** it shares with the
/// house next door. Measured over 1,289 doors in that cell: 41% of them were
/// drawn inside the neighbouring building, where the neighbour's own shell hides
/// them completely, and only 17% opened the collider — `Footprint::in_a_doorway`
/// is a radius around this point, and from 1.8 m inside the building that circle
/// never reaches any wall, so a door you *could* see was usually still not one
/// you could walk through.
///
/// So: take the nearest wall whose outside is not inside another building, and
/// put the point **on** it. That takes visible doors to 99% and, because the
/// point now lies in the wall, the collider gap straddles it by construction. A
/// variant that also weighed each wall's distance to the BGT's pavement and
/// road surfaces was measured against this one and reached 95% street-facing
/// against 94% — 1.6 points, for a 1.2 MB per-cell dependency inside the mesh
/// job, so it is not what this does.
///
/// Two limits worth knowing. The *along-wall* position is still only the unit's,
/// so the doorway lands where the facade is nearest the unit rather than at the
/// real leaf — which is why `collision::DOOR_HALF_WIDTH` stays generous. And a
/// building's neighbours are only the ones in the same cell file, so a door on a
/// party wall crossing a cell boundary can still be buried; that is a thin strip
/// of a 6 km cell and it fails the way it did before rather than worse.
fn place_doors_on_facades(buildings: &mut HashMap<Arc<str>, BuildingFact>) {
    /// Grid cell for the neighbour lookup, metres. A few buildings deep, so the
    /// 3×3 neighbourhood a probe consults is small and always sufficient.
    const BUCKET_M: f64 = 50.0;

    // A local metre frame. Any reference latitude inside the cell will do — every
    // distance measured here is tens of metres — so the first ring supplies it.
    let Some(lat0) = buildings
        .values()
        .find_map(|b| b.footprint.first().map(|p| p.0))
    else {
        return;
    };
    let m_lat = 111_320.0_f64;
    let m_lng = 111_320.0_f64 * lat0.to_radians().cos();
    let to_m = |(lat, lng): (f64, f64)| (lng * m_lng, lat * m_lat);
    let to_deg = |(x, y): (f64, f64)| (y / m_lat, x / m_lng);

    // Every ring once, in metres, plus a bucket index over their bounding boxes.
    let rings: HashMap<Arc<str>, Vec<(f64, f64)>> = buildings
        .iter()
        .filter(|(_, b)| b.footprint.len() >= 3)
        .map(|(id, b)| (id.clone(), b.footprint.iter().map(|&p| to_m(p)).collect()))
        .collect();
    let mut buckets: HashMap<(i64, i64), Vec<&Arc<str>>> = HashMap::new();
    for (id, ring) in &rings {
        let (mut lo, mut hi) = (ring[0], ring[0]);
        for &(x, y) in ring {
            lo = (lo.0.min(x), lo.1.min(y));
            hi = (hi.0.max(x), hi.1.max(y));
        }
        for bx in (lo.0 / BUCKET_M).floor() as i64..=(hi.0 / BUCKET_M).floor() as i64 {
            for by in (lo.1 / BUCKET_M).floor() as i64..=(hi.1 / BUCKET_M).floor() as i64 {
                buckets.entry((bx, by)).or_default().push(id);
            }
        }
    }
    let in_a_neighbour = |mine: &Arc<str>, p: (f64, f64)| -> bool {
        let (bx, by) = (
            (p.0 / BUCKET_M).floor() as i64,
            (p.1 / BUCKET_M).floor() as i64,
        );
        for ox in -1..=1 {
            for oy in -1..=1 {
                for id in buckets.get(&(bx + ox, by + oy)).into_iter().flatten() {
                    if *id != mine && rings.get(*id).is_some_and(|r| point_in_ring(r, p)) {
                        return true;
                    }
                }
            }
        }
        false
    };

    for (id, building) in buildings.iter_mut() {
        let Some(ring) = rings.get(id) else {
            continue;
        };
        for door in &mut building.doors {
            if let Some(on_wall) = doorway_on_facade(ring, to_m(*door), |p| in_a_neighbour(id, p)) {
                *door = to_deg(on_wall);
            }
        }
    }
}

/// Where on `ring` the doorway for the register point `p` is, or `None` when no
/// wall of this building can hold one.
///
/// Split out from [`place_doors_on_facades`] so the rule can be tested against a
/// hand-written terrace rather than against a parsed cell.
fn doorway_on_facade(
    ring: &[(f64, f64)],
    p: (f64, f64),
    buried: impl Fn((f64, f64)) -> bool,
) -> Option<(f64, f64)> {
    // Walls, nearest first — so the search stops at the first one that is not a
    // party wall and the neighbour test runs once or twice rather than per wall.
    let mut walls: Vec<(f64, (f64, f64), (f64, f64))> = Vec::with_capacity(ring.len());
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        let ab = (b.0 - a.0, b.1 - a.1);
        let len = (ab.0 * ab.0 + ab.1 * ab.1).sqrt();
        if len < MIN_DOOR_WALL_M {
            continue;
        }
        // Along the wall, clamped clear of both corners: a panel hung across a
        // corner would be half inside the building, and a point sitting exactly
        // on a shared vertex has two nearest walls rather than one.
        let clear = DOOR_CLEARANCE_M.min(len * 0.5 - 0.01) / len;
        let t = (((p.0 - a.0) * ab.0 + (p.1 - a.1) * ab.1) / (len * len)).clamp(clear, 1.0 - clear);
        let at = (a.0 + ab.0 * t, a.1 + ab.1 * t);
        // Outward, decided by probing rather than by the ring's winding — the
        // winding is not promised and getting it backwards is silent (it is what
        // once drew all 113,710 doors on the inside of their own walls).
        let n = (ab.1 / len, -ab.0 / len);
        let n = if point_in_ring(ring, (at.0 + n.0 * PROBE_M, at.1 + n.1 * PROBE_M)) {
            (-n.0, -n.1)
        } else {
            n
        };
        let d2 = (at.0 - p.0).powi(2) + (at.1 - p.1).powi(2);
        walls.push((d2, at, n));
    }
    walls.sort_by(|a, b| a.0.total_cmp(&b.0));
    walls
        .into_iter()
        .find(|(_, at, n)| {
            !buried((
                at.0 + n.0 * NEIGHBOUR_PROBE_M,
                at.1 + n.1 * NEIGHBOUR_PROBE_M,
            ))
        })
        .map(|(_, at, _)| at)
}

/// Even-odd ray cast against a ring of metre pairs.
///
/// A third copy of a test `collision::Footprint` and `lod22` also carry, and
/// deliberately: both of those answer about `Vec2` in the anchored world frame,
/// while this one runs at parse time on `f64` pairs derived straight from
/// lat/lng, before any anchor exists to convert them against.
fn point_in_ring(ring: &[(f64, f64)], p: (f64, f64)) -> bool {
    let mut inside = false;
    let n = ring.len();
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
        if (a.1 > p.1) != (b.1 > p.1) {
            let x = a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0);
            if p.0 < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

// ─── Coverage ────────────────────────────────────────────────────────────────

/// Which cells the host serves BAG attributes for (`systems::map::coverage`).
static COVERAGE: CoverageSlot = CoverageSlot::new("bag", "/bag/coverage.json", BAG_ZOOM);

/// Bumped once, when the coverage question is finally answered — successfully or
/// as the empty "no BAG on this host" fallback.
///
/// `lod22` bakes a building's year and use into vertex colours, so a cell built
/// before this manifest arrived is coloured as if the register said nothing, and
/// nothing would ever revisit it: cells are rebuilt on a re-anchor, and standing
/// still after startup is the normal case. Same shape as `TerrainField::revision`
/// and read in the same retear — and like that field it can only change **once**
/// per process, so the cost is one rebuild of whatever was already standing.
pub fn coverage_revision() -> u32 {
    COVERAGE.revision()
}

/// Start the coverage fetch from a caller that is not the facts streamer.
///
/// `lod22` calls this because it consumes the answer and may well run before the
/// streamer does on the frames that matter (both give up early without a map
/// anchor). Idempotent and cheap.
pub(crate) fn prefetch_coverage(now: f64) {
    COVERAGE.ensure(now);
}

/// True if the host serves BAG attributes for this cell. `false` until coverage
/// lands, so every consumer degrades to "no facts" rather than waiting.
pub fn bag_covers(cx: i64, cy: i64) -> bool {
    COVERAGE.covers(cx, cy)
}

/// The BAG attribution line, once coverage has loaded and is non-empty.
pub fn bag_attribution() -> Option<String> {
    COVERAGE.attribution()
}

// ─── Fetching ────────────────────────────────────────────────────────────────

/// Which of a cell's two files is wanted.
///
/// They carry the same schema and different arrays. The split is measured, not
/// tidiness: on Groningen's densest z14 cell the 8 000 buildings are 0.53 MB and
/// the 17 785 addressed units are 2.04 MB, and `lod22` — which fetches a cell
/// inside its own mesh job, because a year and a use are baked into vertex
/// colours — has no use at all for the addresses. One file made the heaviest
/// streaming lane in the client pull five times what it reads.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BagPart {
    /// `y.json` — one row per building. What the mesh lane needs.
    Buildings,
    /// `y.places.json` — addressed units and berths, with their street table.
    Places,
}

impl BagPart {
    fn suffix(self) -> &'static str {
        match self {
            Self::Buildings => "json",
            Self::Places => "places.json",
        }
    }
}

/// Cache key for one of a cell's BAG files.
///
/// **The `v2` is a format revision and it is the reason doors ever appear.** This
/// key had no version, and the cache has no revalidation: a hit is served and the
/// host is never asked. So a client that loaded a city before the extractor
/// started shipping `doors` and `fp` — the BAG cells were regenerated with them on
/// 2026-08-29 — held a doorless copy of that cell **forever**, and no amount of
/// updating the app could dislodge it. Both new fields are `#[serde(default)]`, so
/// nothing fails: the cell parses, every building comes out with no entrance and
/// no outline, `lod22` draws no door, `interiors` builds no room, and the map
/// looks exactly as it did before any of it was written. That is what a published
/// APK carrying the fix looked like on a phone that had already been to Groningen.
///
/// `lod22` learned this the same way and carries the same `v2` for the same
/// reason. Bump it whenever a cell gains a field a reader would miss.
fn cell_cache_key(cx: i64, cy: i64, part: BagPart) -> String {
    format!("buildings/bag/v2/{BAG_ZOOM}/{cx}/{cy}.{}", part.suffix())
}

/// Fetch one of a cell's attribute files, through the storage cache.
///
/// `pub(crate)` because `lod22` calls it from inside its own mesh job — see the
/// module header.
pub(crate) async fn fetch_bag_text(cx: i64, cy: i64, part: BagPart) -> Result<String, String> {
    let key = cell_cache_key(cx, cy, part);
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        return Ok(cached);
    }
    // **One download per file, however many ask.** `lod22` fetches a cell's
    // buildings inside its own job while this module's stream asks for the same
    // file, and on a cold start both miss the cache in the same instant: the
    // "second request that is a cache hit" was a second full download (Deck,
    // 2026-09-26: 52 BAG requests, 34 MB, for a 25-cell ring). A caller that finds
    // the file already on its way waits for that answer instead.
    let waiter = {
        let mut flying = match BAG_IN_FLIGHT.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        match flying.get_mut(&key) {
            Some(waiters) => {
                let (tx, rx) = futures_channel::oneshot::channel();
                waiters.push(tx);
                Some(rx)
            }
            None => {
                flying.insert(key.clone(), Vec::new());
                None
            }
        }
    };
    if let Some(rx) = waiter {
        return rx
            .await
            .unwrap_or_else(|_| Err("the shared BAG download was dropped".to_string()));
    }
    let answer = download_bag_text(cx, cy, part, &key).await;
    let waiters = match BAG_IN_FLIGHT.lock() {
        Ok(mut g) => g.remove(&key),
        Err(p) => p.into_inner().remove(&key),
    };
    for tx in waiters.into_iter().flatten() {
        let _ = tx.send(answer.clone());
    }
    answer
}

type BagWaiters = Vec<futures_channel::oneshot::Sender<Result<String, String>>>;

/// The BAG files on their way, by cache key, with whoever else is waiting for each.
static BAG_IN_FLIGHT: crate::platform::sync::LazyLock<
    crate::platform::sync::Mutex<HashMap<String, BagWaiters>>,
> = crate::platform::sync::LazyLock::new(|| crate::platform::sync::Mutex::new(HashMap::new()));

async fn download_bag_text(cx: i64, cy: i64, part: BagPart, key: &str) -> Result<String, String> {
    let url = format!(
        "{}/bag/{}/{}/{}.{}",
        tdbag_url(),
        BAG_ZOOM,
        cx,
        cy,
        part.suffix()
    );
    let bytes = http::fetch(http::Request::get(url)).await?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    storage::put(storage::Namespace::Cache, key, text.as_bytes()).await;
    Ok(text)
}

/// Fetch and parse one cell's **buildings** for a caller that already has a worker
/// to do it on (`lod22`). Returns an empty cell for an uncovered or unreachable
/// one — a consumer's fallback is always "no facts", never an error to handle.
///
/// A cell with no buildings at all has no file, and a 404 is the ordinary answer
/// for open country inside the coverage rectangle rather than a fault.
pub(crate) async fn load_bag_cell(cx: i64, cy: i64) -> Arc<BagCell> {
    if !bag_covers(cx, cy) {
        return Arc::default();
    }
    // Parsed on a worker (`workers::compute`): on the web this future is on the
    // main thread, and a dense cell's buildings file is 1.9 MB of JSON.
    match fetch_bag_text(cx, cy, BagPart::Buildings).await {
        Ok(text) => crate::platform::workers::compute("bag_cell", move || {
            parse_part(cx, cy, BagPart::Buildings, &text)
        })
        .await
        .flatten()
        .unwrap_or_default(),
        Err(_) => Arc::default(),
    }
}

/// How many parsed files are kept alive after their last reader lets go, so a
/// reader arriving a moment later still finds one.
const RECENT_PARSED: usize = 8;

/// Every BAG file parsed this session that somebody still holds, by cache key,
/// plus the last few whether held or not.
#[derive(Default)]
struct ParsedFiles {
    live: HashMap<String, std::sync::Weak<BagCell>>,
    recent: std::collections::VecDeque<Arc<BagCell>>,
}

static PARSED: crate::platform::sync::LazyLock<crate::platform::sync::Mutex<ParsedFiles>> =
    std::sync::LazyLock::new(Default::default);

/// Parse one of a cell's files, **once however many read it**.
///
/// Three readers want the same files: this module's streamer (both), `lod22`'s
/// mesh job (the buildings, for the colours it bakes) and `map_geometry`'s
/// moorings (the places, for the berths, and the buildings again, to tell a
/// houseboat under a modelled building). Each used to parse its own copy, so a
/// cell's buildings file — 1.9 MB of JSON on Groningen's densest — was parsed up
/// to three times on every start. The download was already shared
/// (`BAG_IN_FLIGHT`); this shares the result.
///
/// Held by `Weak`, so a parse lives exactly as long as a reader keeps it (the
/// streamer keeps its ring's), plus the last [`RECENT_PARSED`] — which covers the
/// usual race, two readers asking within the same second. A failed parse is not
/// remembered: it is `None` to every caller, as `parse_bag_cell` failing was.
/// Two readers parsing the same file at the same instant both parse it, which is
/// the old cost and no worse.
pub(crate) fn parse_part(cx: i64, cy: i64, part: BagPart, text: &str) -> Option<Arc<BagCell>> {
    let key = cell_cache_key(cx, cy, part);
    let lock = || match PARSED.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some(hit) = lock().live.get(&key).and_then(std::sync::Weak::upgrade) {
        return Some(hit);
    }
    let parsed = Arc::new(parse_bag_cell(text).ok()?);
    let mut files = lock();
    files.live.retain(|_, w| w.strong_count() > 0);
    files.live.insert(key, Arc::downgrade(&parsed));
    files.recent.push_back(parsed.clone());
    while files.recent.len() > RECENT_PARSED {
        files.recent.pop_front();
    }
    Some(parsed)
}

/// A cell's registered houseboat berths (`ligplaats`), as lat/lng, each with
/// whether a registered building stands on it — for `map_geometry`, which floats
/// a houseboat on each (`moorings::place_houseboats`) and draws only the hull
/// under a building 3DBAG already models. Reads the places and buildings files
/// `building_facts` and `lod22` stream anyway, through the same shared
/// downloads. Empty for an uncovered or unreachable cell.
pub(crate) async fn load_berths(cx: i64, cy: i64) -> Vec<((f64, f64), bool)> {
    if !bag_covers(cx, cy) {
        return Vec::new();
    }
    let Ok(text) = fetch_bag_text(cx, cy, BagPart::Places).await else {
        return Vec::new();
    };
    let places = crate::platform::workers::compute("bag_places", move || {
        parse_part(cx, cy, BagPart::Places, &text)
    })
    .await
    .flatten();
    let berths: Vec<(f64, f64)> = places
        .map(|c| {
            c.berths
                .iter()
                .filter(|b| b.kind == BerthKind::Mooring)
                .map(|b| b.at)
                .collect()
        })
        .unwrap_or_default();
    if berths.is_empty() {
        return Vec::new();
    }
    let buildings = load_bag_cell(cx, cy).await;
    let housed = |p: (f64, f64)| {
        buildings
            .buildings
            .values()
            .any(|b| b.footprint.len() >= 3 && point_in_latlng_ring(&b.footprint, p))
    };
    berths.into_iter().map(|p| (p, housed(p))).collect()
}

/// Even-odd point-in-ring over lat/lng, for rings the size of a building.
fn point_in_latlng_ring(ring: &[(f64, f64)], p: (f64, f64)) -> bool {
    let mut inside = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.0 > p.0) != (b.0 > p.0) && p.1 < (b.1 - a.1) * (p.0 - a.0) / (b.0 - a.0) + a.1 {
            inside = !inside;
        }
    }
    inside
}

// ─── The resource and its streamer ───────────────────────────────────────────

/// One loaded cell: its two files as parsed, either absent when the host has none.
type CellFiles = (Option<Arc<BagCell>>, Option<Arc<BagCell>>);

/// Every BAG fact the client is currently holding.
///
/// Held per cell, as parsed, rather than copied into one index: the parse is
/// shared with `lod22` and the moorings ([`parse_part`]), and a caller with an
/// id looks it up across the ring's 25 cells, which is 25 hash lookups.
#[derive(Resource)]
pub struct BuildingFacts {
    stream: CellStream<(i64, i64), Result<CellFiles, String>>,
    cells: HashMap<(i64, i64), CellFiles>,
    anchor: Option<(i64, i64, u32, f64)>,
}

impl Default for BuildingFacts {
    fn default() -> Self {
        Self {
            stream: CellStream::new(Policy {
                max_in_flight: BAG_CONCURRENCY,
                min_interval_secs: 0.0,
                max_attempts: BAG_MAX_ATTEMPTS,
                backoff_secs: BAG_BACKOFF_SECS,
                backoff_doublings: BAG_BACKOFF_DOUBLINGS,
                cooldown_secs: BAG_RETRY_AFTER_SECS,
                // Nothing here is an entity, so the despawn queue is unused; the
                // policy field still has to say something and one is the honest
                // answer for "there is never a backlog".
                despawns_per_frame: 1,
            }),
            cells: HashMap::new(),
            anchor: None,
        }
    }
}

/// Only ever busy over covered ground below the globe tier: nothing is requested
/// for a cell the host does not serve, and a stream that was never asked is quiet.
impl crate::systems::stream_watch::StreamBusy for BuildingFacts {
    const NAME: &'static str = "building_facts";
    fn stream_busy(&self) -> bool {
        self.stream.busy()
    }
}

impl BuildingFacts {
    /// Each loaded cell's buildings file.
    fn building_files(&self) -> impl Iterator<Item = &BagCell> {
        self.cells.values().filter_map(|(b, _)| b.as_deref())
    }

    /// Each loaded cell's places file.
    fn place_files(&self) -> impl Iterator<Item = &BagCell> {
        self.cells.values().filter_map(|(_, p)| p.as_deref())
    }

    /// What the register says about a building, if its cell is loaded.
    pub fn building(&self, id: &str) -> Option<&BuildingFact> {
        self.building_files().find_map(|c| c.building(id))
    }

    /// [`Self::building`] with the id as the index holds it, in the shape
    /// [`Self::near`] answers in.
    pub fn building_entry(&self, id: &str) -> Option<(&str, &BuildingFact)> {
        self.building_files()
            .find_map(|c| c.buildings.get_key_value(id))
            .map(|(k, v)| (&**k, v))
    }

    /// Every addressed unit currently loaded.
    pub fn addresses(&self) -> impl Iterator<Item = &AddressFact> {
        self.place_files().flat_map(|c| &c.addresses)
    }

    /// Every houseboat berth and mobile-home plot currently loaded.
    pub fn berths(&self) -> impl Iterator<Item = &BerthFact> {
        self.place_files().flat_map(|c| &c.berths)
    }

    /// How many buildings are indexed — the cheap "is there anything here?" test,
    /// and what `--dump-state` reports.
    pub fn len(&self) -> usize {
        self.building_files().map(|c| c.buildings.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.building_files().all(|c| c.buildings.is_empty())
    }

    /// Every building whose outline starts within `max_m` of a (lat, lng), with
    /// its identificatie — the key the caller joins the LoD2.2 shell height on
    /// (`lod22::Lod22Buildings::shell_height`), which is what says how many
    /// storeys fit inside.
    ///
    /// Linear over the index — tens of thousands of buildings, walked when the
    /// avatar has moved fifteen metres rather than per frame, which is a fraction
    /// of a millisecond in the place it is spent. Distance is measured to the
    /// outline's **first vertex** rather than to a centroid: a building is metres
    /// across and this radius is seventy, so the difference cannot change an
    /// answer that matters, and a centroid would cost a pass over every ring.
    pub fn near(&self, lat: f64, lng: f64, max_m: f64) -> Vec<(&str, &BuildingFact)> {
        const M_PER_DEG: f64 = 111_320.0;
        let scale = lat.to_radians().cos();
        let max2 = max_m * max_m;
        self.building_files()
            .flat_map(|c| &c.buildings)
            .map(|(id, b)| (&**id, b))
            .filter(|(_, b)| {
                let Some(&(blat, blng)) = b.footprint.first() else {
                    return false;
                };
                let dy = (blat - lat) * M_PER_DEG;
                let dx = (blng - lng) * M_PER_DEG * scale;
                dx * dx + dy * dy <= max2
            })
            .collect()
    }

    /// The door nearest a point **inside** a building: where a walk to that
    /// building ends (`navigation::snap_walk_to_door`).
    ///
    /// `None` when the point is in no loaded building, or the building has no
    /// door — a destination on a square, in a park or at a person is left alone,
    /// which is the point of asking "inside?" rather than "near?". A building's
    /// first vertex is within 300 m of any point inside it for all but the very
    /// largest, which is the radius [`Self::near`] is asked with.
    #[must_use]
    pub fn door_for(&self, lat: f64, lng: f64) -> Option<(f64, f64)> {
        let scale = lat.to_radians().cos();
        let d2 = |p: &(f64, f64)| {
            let dy = p.0 - lat;
            let dx = (p.1 - lng) * scale;
            dx * dx + dy * dy
        };
        self.near(lat, lng, 300.0)
            .into_iter()
            .find(|(_, b)| crate::systems::osm_buildings::point_in_ring((lat, lng), &b.footprint))
            .and_then(|(_, b)| {
                b.doors
                    .iter()
                    .copied()
                    .min_by(|a, b| d2(a).total_cmp(&d2(b)))
            })
    }

    /// The nearest addressed unit to a (lat, lng), within `max_m` metres.
    ///
    /// Linear over the loaded set: a ring of cells holds a few thousand addresses
    /// and this answers an interaction, not a frame. Distance is the flat
    /// small-angle approximation — over one z14 cell the error is far below the
    /// register's own placement.
    pub fn nearest_address(&self, lat: f64, lng: f64, max_m: f64) -> Option<&AddressFact> {
        const M_PER_DEG: f64 = 111_320.0;
        let scale = lat.to_radians().cos();
        let mut best: Option<(f64, &AddressFact)> = None;
        for a in self.addresses() {
            let dy = (a.at.0 - lat) * M_PER_DEG;
            let dx = (a.at.1 - lng) * M_PER_DEG * scale;
            let d2 = dx * dx + dy * dy;
            if d2 <= max_m * max_m && best.is_none_or(|(b, _)| d2 < b) {
                best = Some((d2, a));
            }
        }
        best.map(|(_, a)| a)
    }

    fn insert_cell(&mut self, cell: (i64, i64), files: CellFiles) {
        self.cells.insert(cell, files);
    }

    /// Drop everything — on a re-anchor, or when the ring moves far enough that
    /// rebuilding the index beats reconciling it.
    fn clear(&mut self) {
        self.cells.clear();
    }
}

/// Stream the BAG cells around the camera into [`BuildingFacts`].
///
/// Deliberately dumb next to `update_lod22`: there is nothing to spawn, nothing to
/// upload and nothing to despawn, so there is no ledger, no per-frame build cap
/// and no partial application. The one thing it shares is the ring and the
/// re-anchor teardown.
pub fn update_building_facts(
    mut facts: ResMut<BuildingFacts>,
    stream: Res<MapStream>,
    camera: AnchoredCamera,
    globe: Res<GlobeState>,
    time: Res<Time>,
) {
    let now = time.elapsed_secs_f64();
    COVERAGE.ensure(now);

    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };

    // A re-anchor invalidates nothing about the *facts* — a building's year does
    // not depend on where the map is pinned — but the ring is expressed in cells
    // around the old anchor, so the bookkeeping has to start over. The index is
    // rebuilt from the cells that come back.
    if facts.stream.sync(stream.anchor_generation) {
        facts.stream.reset();
        facts.clear();
        facts.anchor = None;
    }

    if facts.anchor.is_none() {
        let (ax, ay) = lat_lng_to_tile(clat, clng, base_zoom);
        facts.anchor = Some((ax, ay, base_zoom, clat));
    }
    let (ax, ay, z, lat) = facts.anchor.expect("set just above");
    let tile_m = tile_to_meters(z, lat) as f32;

    let Some((cell, tf)) = camera.get() else {
        return;
    };
    // Freeze from orbit, like every other cell streamer: you cannot read a
    // house number from 60 km up, and the ring would chase the camera across a
    // continent.
    if globe.altitude_m(cell, tf) as f32 > GLOBE_SHOW_ALT {
        return;
    }

    let centre = cell_ring::coarsen(
        cell_ring::camera_tile((ax, ay), tile_m, cell, tf),
        z,
        BAG_ZOOM,
    );
    cell_ring::request_ring(&mut facts.stream, centre, BAG_RADIUS, now, |c, _| {
        bag_covers(c.0, c.1)
    });

    let sink = facts.stream.sink();
    while let Some((cx, cy)) = facts.stream.next_job(now) {
        let sink = sink.clone();
        IoTaskPool::get()
            .spawn(async move {
                // Both files, and **neither is required**: a cell can hold
                // buildings and no addresses (an industrial estate) or the
                // reverse (a canal lined with berths), and either file is simply
                // absent then. Failing the cell on one missing half would drop
                // the other for no reason, so a miss is an empty string and the
                // cell fails only when both are gone.
                let buildings = fetch_bag_text(cx, cy, BagPart::Buildings).await;
                let places = fetch_bag_text(cx, cy, BagPart::Places).await;
                if let (Err(e), Err(_)) = (&buildings, &places) {
                    if let Ok(mut g) = sink.lock() {
                        g.push(((cx, cy), Err(e.clone())));
                    }
                    return;
                }
                // Parse on the worker pool, not here: on wasm this future runs on
                // the main thread, and a dense cell is megabytes of JSON.
                crate::platform::workers::submit(
                    "bag_facts",
                    Box::new(move || {
                        let files = (
                            buildings
                                .ok()
                                .and_then(|t| parse_part(cx, cy, BagPart::Buildings, &t)),
                            places
                                .ok()
                                .and_then(|t| parse_part(cx, cy, BagPart::Places, &t)),
                        );
                        if let Ok(mut g) = sink.lock() {
                            g.push(((cx, cy), Ok(files)));
                        }
                    }),
                );
            })
            .detach();
    }

    for (c, result) in facts.stream.take_results() {
        match result {
            Ok(parsed) => {
                facts.stream.succeeded(&c);
                facts.insert_cell(c, parsed);
            }
            Err(e) => {
                let attempts = facts.stream.failed(&c, now);
                if facts.stream.exhausted(&c) {
                    tracing::debug!(cx = c.0, cy = c.1, attempts, error = %e,
                                    "BAG cell unavailable");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_cell_and_interns_its_streets() {
        let cell = parse_bag_cell(
            r#"{"streets":["Paterswoldseweg","Kleine Badstraat"],
                "buildings":[{"id":"0014100010909097","year":1884,"use":3,
                              "area":94,"units":1},
                             {"id":"0014100010909098","status":"demolished"}],
                "addresses":[{"at":[53.2,6.5],"id":"0014100010909097","street":0,
                              "nr":"62","postcode":"9726BG","use":1,"area":156}],
                "berths":[{"kind":"mooring","at":[53.21,6.55],"street":1,"nr":"3"}]}"#,
        )
        .expect("valid cell");

        let b = cell.building("0014100010909097").expect("indexed by id");
        assert_eq!(b.year, Some(1884));
        assert_eq!(b.uses, uses::HOME | uses::SHOP);
        assert_eq!(b.area_m2, Some(94));
        assert_eq!(b.status, BuildingStatus::InUse);
        assert!(b.status.standing());

        let gone = cell.building("0014100010909098").expect("indexed by id");
        assert_eq!(gone.status, BuildingStatus::Demolished);
        assert!(!gone.status.standing());
        assert_eq!(gone.year, None);

        assert_eq!(
            cell.addresses[0].label().as_deref(),
            Some("Paterswoldseweg 62")
        );
        assert_eq!(cell.addresses[0].pand.as_deref(), Some("0014100010909097"));
        assert_eq!(cell.berths[0].kind, BerthKind::Mooring);
        assert_eq!(cell.berths[0].street.as_deref(), Some("Kleine Badstraat"));
    }

    /// Every field but `id`/`at`/`kind` is omitted when the register has nothing
    /// to say, so the parser has to read a minimal record rather than reject it.
    #[test]
    fn absent_fields_are_absent_not_fatal() {
        let cell = parse_bag_cell(
            r#"{"buildings":[{"id":"0014100010909097"}],
                "addresses":[{"at":[53.2,6.5]}]}"#,
        )
        .expect("valid cell");
        let b = cell.building("0014100010909097").expect("indexed");
        assert_eq!(b.uses, 0);
        assert_eq!(b.units, 0);
        assert_eq!(b.year, None);
        assert!(use_label(b.uses).is_none());
        assert!(cell.addresses[0].label().is_none());
        assert!(cell.addresses[0].pand.is_none());
    }

    /// A street index the file does not carry must not panic — the intern table
    /// and the records are written by the same tool, but a truncated cache entry
    /// is a real thing to survive.
    #[test]
    fn an_out_of_range_street_index_is_dropped() {
        let cell = parse_bag_cell(
            r#"{"streets":["Only One"],
                "addresses":[{"at":[53.2,6.5],"street":7,"nr":"1"}]}"#,
        )
        .expect("valid cell");
        assert!(cell.addresses[0].street.is_none());
        assert_eq!(cell.addresses[0].label().as_deref(), Some("1"));
    }

    #[test]
    fn a_placeholder_year_of_zero_is_not_a_year() {
        let cell = parse_bag_cell(r#"{"buildings":[{"id":"x","year":0}]}"#).expect("valid cell");
        assert_eq!(cell.building("x").expect("indexed").year, None);
    }

    #[test]
    fn use_label_names_the_lowest_bit_set() {
        assert_eq!(use_label(uses::HOME | uses::OFFICE), Some("home"));
        assert_eq!(use_label(uses::OFFICE), Some("office"));
        assert_eq!(use_label(0), None);
    }

    #[test]
    fn a_file_is_parsed_once_however_many_read_it() {
        // A cell number no other test uses, since the memo is process-wide.
        let text = r#"{"buildings":[{"id":"a","year":1900}]}"#;
        let first = parse_part(-7, -7, BagPart::Buildings, text).expect("parses");
        let second = parse_part(-7, -7, BagPart::Buildings, text).expect("parses");
        assert!(
            Arc::ptr_eq(&first, &second),
            "the second reader parsed again"
        );
        let places = parse_part(-7, -7, BagPart::Places, text).expect("parses");
        assert!(
            !Arc::ptr_eq(&first, &places),
            "the other file of the same cell is a different parse"
        );
        assert!(parse_part(-8, -8, BagPart::Buildings, "not json").is_none());
    }

    /// A walk ends at the door of the building its destination is *in*, and
    /// nowhere else: a point on the square outside is not moved to anybody's
    /// front door.
    #[test]
    fn a_door_is_found_only_from_inside_its_building() {
        let mut cell = BagCell::default();
        cell.buildings.insert(
            Arc::from("NL.IMBAG.Pand.1"),
            BuildingFact {
                // ~20 m × ~22 m around (53.2000, 6.5000).
                footprint: vec![
                    (53.1999, 6.4998),
                    (53.1999, 6.5002),
                    (53.2001, 6.5002),
                    (53.2001, 6.4998),
                ],
                doors: vec![(53.1999, 6.5001), (53.2001, 6.4999)],
                ..BuildingFact::default()
            },
        );
        let mut facts = BuildingFacts::default();
        facts.insert_cell((0, 0), (Some(Arc::new(cell)), None));
        // Inside, nearer the south door.
        assert_eq!(facts.door_for(53.19995, 6.5001), Some((53.1999, 6.5001)));
        // Inside, nearer the north door.
        assert_eq!(facts.door_for(53.20005, 6.4999), Some((53.2001, 6.4999)));
        // Out on the street beside it: left alone.
        assert_eq!(facts.door_for(53.1997, 6.5000), None);
    }

    #[test]
    fn nearest_address_respects_its_radius() {
        let mut facts = BuildingFacts::default();
        facts.insert_cell(
            (0, 0),
            (
                None,
                Some(Arc::new(
                    parse_bag_cell(
                        r#"{"streets":["Near","Far"],
                    "addresses":[{"at":[53.2000,6.5000],"street":0,"nr":"1"},
                                 {"at":[53.2100,6.5000],"street":1,"nr":"2"}]}"#,
                    )
                    .expect("valid cell"),
                )),
            ),
        );
        // ~11 m away: inside a 50 m radius, and the nearer of the two.
        let hit = facts
            .nearest_address(53.2001, 6.5000, 50.0)
            .expect("within radius");
        assert_eq!(hit.number.as_deref(), Some("1"));
        // Standing midway between them — ~550 m from each — a 5 m radius finds
        // neither, which is the answer that matters: a readout that reaches for
        // the nearest address however far away it is would name the wrong street
        // in the middle of a field.
        assert!(facts.nearest_address(53.2050, 6.5000, 5.0).is_none());
    }

    /// A terrace: three 6 m × 10 m houses in a row, the middle one's unit point
    /// 1.5 m in from its own front wall — and therefore *nearer* the party wall
    /// it shares with its neighbour. The nearest wall is the wrong answer; the
    /// nearest wall with nobody behind it is the right one.
    #[test]
    fn a_doorway_never_lands_on_a_party_wall() {
        // Metres, x east and y north. Fronts run along y = 0.
        let house = |x0: f64| vec![(x0, 0.0), (x0 + 6.0, 0.0), (x0 + 6.0, 10.0), (x0, 10.0)];
        let (left, middle, right) = (house(-6.0), house(0.0), house(6.0));
        let neighbours = |p: (f64, f64)| point_in_ring(&left, p) || point_in_ring(&right, p);

        // 1.0 m from the party wall at x = 6, 1.5 m from the front wall at y = 0.
        let p = (5.0, 1.5);
        let nearest = doorway_on_facade(&middle, p, |_| false).expect("a wall");
        assert!(
            (nearest.0 - 6.0).abs() < 0.01,
            "without the neighbours the nearest wall is the party wall, got {nearest:?}"
        );

        let at = doorway_on_facade(&middle, p, neighbours).expect("a wall");
        assert!(
            at.1.abs() < 0.01,
            "the doorway should be on the front wall (y = 0), got {at:?}"
        );
        assert!(
            (at.0 - 5.0).abs() < 0.01,
            "and at the point of that wall nearest the unit, got {at:?}"
        );
    }

    /// The whole point of moving the point: it has to end up *in* the wall, or
    /// `collision::Footprint`'s radius never straddles it and the door cannot be
    /// walked through.
    #[test]
    fn a_doorway_lands_on_the_ring_itself() {
        let ring = vec![(0.0, 0.0), (12.0, 0.0), (12.0, 8.0), (0.0, 8.0)];
        for p in [(3.0, 2.5), (11.0, 4.0), (6.0, 7.2), (0.5, 0.5)] {
            let at = doorway_on_facade(&ring, p, |_| false).expect("a wall");
            let on_edge = at.1.abs() < 1e-9
                || (at.1 - 8.0).abs() < 1e-9
                || at.0.abs() < 1e-9
                || (at.0 - 12.0).abs() < 1e-9;
            assert!(
                on_edge,
                "{p:?} projected to {at:?}, which is not on the ring"
            );
        }
    }

    /// A door hung across a corner would be half inside its own building, and a
    /// point exactly on a shared vertex has two nearest walls rather than one —
    /// which is the ambiguity `lod22` then has to resolve to pick a normal.
    #[test]
    fn a_doorway_keeps_clear_of_the_corners() {
        let ring = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let at = doorway_on_facade(&ring, (0.2, 0.2), |_| false).expect("a wall");
        assert!(
            at.0 >= DOOR_CLEARANCE_M - 1e-9 && at.1.abs() < 1e-9,
            "a point in the corner should slide along the wall, got {at:?}"
        );
    }

    /// Boxed in on every side, the honest answer is that this building has no
    /// doorway — not one on a wall the neighbour hides.
    #[test]
    fn a_building_with_no_free_wall_gets_no_doorway() {
        let ring = vec![(0.0, 0.0), (5.0, 0.0), (5.0, 5.0), (0.0, 5.0)];
        assert!(doorway_on_facade(&ring, (2.5, 2.5), |_| true).is_none());
    }

    /// A wall too short to hold the panel is not a candidate, whatever its
    /// distance — the chamfered corner of a building is not its front door.
    #[test]
    fn a_wall_shorter_than_a_door_is_not_a_candidate() {
        // A 10 m square with its north-east corner chamfered by 0.5 m.
        let ring = vec![
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 9.5),
            (9.5, 10.0),
            (0.0, 10.0),
        ];
        let at = doorway_on_facade(&ring, (9.4, 9.4), |_| false).expect("a wall");
        let on_chamfer = (at.0 + at.1 - 19.5).abs() < 0.01;
        assert!(
            !on_chamfer,
            "the 0.7 m chamfer cannot hold a door, got {at:?}"
        );
    }

    /// The parse runs the placement, so a cell's doors come out on their walls.
    #[test]
    fn parsing_a_cell_puts_its_doors_on_the_facade() {
        let cell = parse_bag_cell(
            r#"{"streets":[],"buildings":[
                 {"id":"a","fp":[[53.2000000,6.5000000],[53.2000000,6.5001000],
                                 [53.2000900,6.5001000],[53.2000900,6.5000000]],
                  "doors":[[53.2000450,6.5000500]]}],
                "addresses":[],"berths":[]}"#,
        )
        .expect("valid cell");
        let b = cell.building("a").expect("indexed by id");
        let (lat, lng) = b.doors[0];
        let on_edge = (lat - 53.2000000).abs() < 1e-7
            || (lat - 53.2000900).abs() < 1e-7
            || (lng - 6.5000000).abs() < 1e-7
            || (lng - 6.5001000).abs() < 1e-7;
        assert!(on_edge, "door came out at {lat},{lng}, not on the ring");
    }
}
