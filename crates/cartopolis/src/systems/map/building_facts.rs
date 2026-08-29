//! What the BAG says about a building we are already drawing.
//!
//! The 3DBAG lanes supply a building's shape; this supplies its *facts* — year
//! built, what it is used for, how big it is, whether the register still thinks
//! it exists — plus the addresses and the houseboat berths in the same cell.
//! Served from our own host by `tools/bag_attributes.py` as
//! `bag/{14}/{x}/{y}.json`, alongside the 3DBAG trees and behind the same
//! coverage manifest pattern.
//!
//! **The join is an id, not a geometry test.** `tools/3dbag_lod22.py` writes each
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
//! [`BuildingFacts`] and the streamer below are for. Sharing one parse would mean
//! rebuilding meshes whenever a facts cell landed, which is the terrain-revision
//! retear — affordable for a 6.4 km field that arrives once, not for a stream.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;

use crate::components::FreeCamera;
use crate::grid::WORLD_CELL_EDGE;
use crate::platform::stream::{CellStream, Policy};
use crate::platform::{http, storage};
use crate::systems::coverage::CoverageSlot;
use crate::systems::globe::{GlobeState, GLOBE_SHOW_ALT};
use crate::systems::map_stream::MapStream;
use crate::systems::osm_buildings::tdbag_url;
use big_space::prelude::GridCell;
use cartopolis_core::world::{lat_lng_to_tile, tile_to_meters};

/// Fixed cache/streaming zoom — must match `osm_buildings::CACHE_ZOOM` and the
/// `CACHE_ZOOM` the extractor bins into.
const BAG_ZOOM: u32 = 14;

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
/// and is mirrored by `USE_BITS` in `tools/bag_attributes.py`: **append, never
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
    /// Where this building's addressed units are entered, as lat/lng. The
    /// register's own points, which is the closest thing to a national
    /// door-position dataset there is.
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
    #[derive(serde::Deserialize)]
    struct WireCell {
        #[serde(default)]
        streets: Vec<String>,
        #[serde(default)]
        districts: Vec<String>,
        #[serde(default)]
        buildings: Vec<WireBuilding>,
        #[serde(default)]
        addresses: Vec<WireAddress>,
        #[serde(default)]
        berths: Vec<WireBerth>,
    }
    #[derive(serde::Deserialize)]
    struct WireBuilding {
        id: String,
        #[serde(default)]
        year: Option<u16>,
        #[serde(default)]
        #[serde(rename = "use")]
        uses: u16,
        #[serde(default)]
        area: Option<u32>,
        #[serde(default)]
        area_max: Option<u32>,
        #[serde(default)]
        units: u16,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        mon: Option<u32>,
        #[serde(default)]
        mcat: Option<String>,
        #[serde(default)]
        district: Option<usize>,
        #[serde(default)]
        fp: Vec<(f64, f64)>,
        #[serde(default)]
        doors: Vec<(f64, f64)>,
    }
    #[derive(serde::Deserialize)]
    struct WireAddress {
        at: (f64, f64),
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        street: Option<usize>,
        #[serde(default)]
        nr: Option<String>,
        #[serde(default)]
        postcode: Option<String>,
        #[serde(default)]
        #[serde(rename = "use")]
        uses: u16,
        #[serde(default)]
        area: Option<u32>,
    }
    #[derive(serde::Deserialize)]
    struct WireBerth {
        at: (f64, f64),
        kind: String,
        #[serde(default)]
        street: Option<usize>,
        #[serde(default)]
        nr: Option<String>,
        #[serde(default)]
        postcode: Option<String>,
    }

    let wire: WireCell = serde_json::from_str(text).map_err(|e| e.to_string())?;
    // Intern once per cell: a street name is repeated by every unit on it, and
    // handing each `AddressFact` its own `String` would allocate thousands of
    // copies of "Paterswoldseweg" per cell.
    let streets: Vec<Arc<str>> = wire.streets.into_iter().map(Arc::from).collect();
    let street_of = |ix: Option<usize>| ix.and_then(|i| streets.get(i).cloned());
    let districts: Vec<Arc<str>> = wire.districts.into_iter().map(Arc::from).collect();

    // Monument categories are a closed vocabulary of a dozen phrases spread over
    // every listed building in the cell, so they are interned as they are seen
    // rather than shipped as a table: the table would have to be built by the
    // extractor for a saving the parser can make for itself.
    let mut categories: HashMap<String, Arc<str>> = HashMap::new();
    let mut buildings = HashMap::with_capacity(wire.buildings.len());
    for b in wire.buildings {
        buildings.insert(
            Arc::from(b.id.as_str()),
            BuildingFact {
                // A year of 0 is a placeholder, not a building from the year nought.
                year: b.year.filter(|y| *y > 0),
                uses: b.uses,
                area_m2: b.area,
                area_max_m2: b.area_max,
                units: b.units,
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
        .into_iter()
        .map(|a| AddressFact {
            at: a.at,
            pand: a.id.map(|s| Arc::from(s.as_str())),
            street: street_of(a.street),
            number: a.nr,
            postcode: a.postcode,
            uses: a.uses,
            area_m2: a.area,
        })
        .collect();
    let berths = wire
        .berths
        .into_iter()
        .map(|b| BerthFact {
            kind: if b.kind == "plot" {
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
    Ok(BagCell {
        buildings,
        addresses,
        berths,
    })
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

fn cell_cache_key(cx: i64, cy: i64, part: BagPart) -> String {
    format!("buildings/bag/{BAG_ZOOM}/{cx}/{cy}.{}", part.suffix())
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
    storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
    Ok(text)
}

/// Fetch and parse one cell's **buildings** for a caller that already has a worker
/// to do it on (`lod22`). Returns an empty cell for an uncovered or unreachable
/// one — a consumer's fallback is always "no facts", never an error to handle.
///
/// A cell with no buildings at all has no file, and a 404 is the ordinary answer
/// for open country inside the coverage rectangle rather than a fault.
pub(crate) async fn load_bag_cell(cx: i64, cy: i64) -> BagCell {
    if !bag_covers(cx, cy) {
        return BagCell::default();
    }
    match fetch_bag_text(cx, cy, BagPart::Buildings).await {
        Ok(text) => parse_bag_cell(&text).unwrap_or_default(),
        Err(_) => BagCell::default(),
    }
}

// ─── The resource and its streamer ───────────────────────────────────────────

/// Every BAG fact the client is currently holding, keyed by building id.
///
/// The per-cell files are the transport; this is the index, because a caller with
/// an id does not know which cell it came from. Cell membership is kept beside it
/// only so eviction can be exact.
#[derive(Resource)]
pub struct BuildingFacts {
    stream: CellStream<(i64, i64), Result<BagCell, String>>,
    /// Ids contributed by each loaded cell, for eviction.
    cell_ids: HashMap<(i64, i64), Vec<Arc<str>>>,
    buildings: HashMap<Arc<str>, BuildingFact>,
    addresses: Vec<AddressFact>,
    berths: Vec<BerthFact>,
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
            cell_ids: HashMap::new(),
            buildings: HashMap::new(),
            addresses: Vec::new(),
            berths: Vec::new(),
            anchor: None,
        }
    }
}

impl BuildingFacts {
    /// What the register says about a building, if its cell is loaded.
    pub fn building(&self, id: &str) -> Option<&BuildingFact> {
        self.buildings.get(id)
    }

    /// Every addressed unit currently loaded.
    pub fn addresses(&self) -> &[AddressFact] {
        &self.addresses
    }

    /// Every houseboat berth and mobile-home plot currently loaded.
    pub fn berths(&self) -> &[BerthFact] {
        &self.berths
    }

    /// How many buildings are indexed — the cheap "is there anything here?" test,
    /// and what `--dump-state` reports.
    pub fn len(&self) -> usize {
        self.buildings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buildings.is_empty()
    }

    /// Every building whose outline starts within `max_m` of a (lat, lng).
    ///
    /// Linear over the index — tens of thousands of buildings, walked when the
    /// avatar has moved fifteen metres rather than per frame, which is a fraction
    /// of a millisecond in the place it is spent. Distance is measured to the
    /// outline's **first vertex** rather than to a centroid: a building is metres
    /// across and this radius is seventy, so the difference cannot change an
    /// answer that matters, and a centroid would cost a pass over every ring.
    pub fn near(&self, lat: f64, lng: f64, max_m: f64) -> Vec<&BuildingFact> {
        const M_PER_DEG: f64 = 111_320.0;
        let scale = lat.to_radians().cos();
        let max2 = max_m * max_m;
        self.buildings
            .values()
            .filter(|b| {
                let Some(&(blat, blng)) = b.footprint.first() else {
                    return false;
                };
                let dy = (blat - lat) * M_PER_DEG;
                let dx = (blng - lng) * M_PER_DEG * scale;
                dx * dx + dy * dy <= max2
            })
            .collect()
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
        for a in &self.addresses {
            let dy = (a.at.0 - lat) * M_PER_DEG;
            let dx = (a.at.1 - lng) * M_PER_DEG * scale;
            let d2 = dx * dx + dy * dy;
            if d2 <= max_m * max_m && best.is_none_or(|(b, _)| d2 < b) {
                best = Some((d2, a));
            }
        }
        best.map(|(_, a)| a)
    }

    fn insert_cell(&mut self, cell: (i64, i64), parsed: BagCell) {
        let mut ids = Vec::with_capacity(parsed.buildings.len());
        for (id, fact) in parsed.buildings {
            ids.push(id.clone());
            self.buildings.insert(id, fact);
        }
        self.cell_ids.insert(cell, ids);
        self.addresses.extend(parsed.addresses);
        self.berths.extend(parsed.berths);
    }

    /// Drop everything — on a re-anchor, or when the ring moves far enough that
    /// rebuilding the index beats reconciling it.
    fn clear(&mut self) {
        self.cell_ids.clear();
        self.buildings.clear();
        self.addresses.clear();
        self.berths.clear();
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
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
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

    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    // Freeze from orbit, like every other cell streamer: you cannot read a
    // house number from 60 km up, and the ring would chase the camera across a
    // continent.
    if globe.altitude_m(cell, tf) as f32 > GLOBE_SHOW_ALT {
        return;
    }

    let world_x = cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x;
    let world_z = cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z;
    let center = (
        ax + (world_x / tile_m).round() as i64,
        ay + (world_z / tile_m).round() as i64,
    );
    let shift = z.saturating_sub(BAG_ZOOM);
    let (ccx, ccy) = (
        center.0.div_euclid(1i64 << shift),
        center.1.div_euclid(1i64 << shift),
    );

    for dy in -BAG_RADIUS..=BAG_RADIUS {
        for dx in -BAG_RADIUS..=BAG_RADIUS {
            let c = (ccx + dx, ccy + dy);
            if bag_covers(c.0, c.1) {
                facts.stream.request(c, now);
            }
        }
    }

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
                        let mut cell = buildings
                            .ok()
                            .and_then(|t| parse_bag_cell(&t).ok())
                            .unwrap_or_default();
                        if let Some(p) = places.ok().and_then(|t| parse_bag_cell(&t).ok()) {
                            cell.addresses = p.addresses;
                            cell.berths = p.berths;
                        }
                        if let Ok(mut g) = sink.lock() {
                            g.push(((cx, cy), Ok(cell)));
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
    fn nearest_address_respects_its_radius() {
        let mut facts = BuildingFacts::default();
        facts.insert_cell(
            (0, 0),
            parse_bag_cell(
                r#"{"streets":["Near","Far"],
                    "addresses":[{"at":[53.2000,6.5000],"street":0,"nr":"1"},
                                 {"at":[53.2100,6.5000],"street":1,"nr":"2"}]}"#,
            )
            .expect("valid cell"),
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
}
