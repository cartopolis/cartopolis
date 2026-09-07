//! **Municipal registers**: the street objects a *city* surveys, where the
//! national ones are silent.
//!
//! `systems::surveyed` reads the BGT, which is national and is the right default
//! precisely because it is. What it is not is complete: its point collections are
//! *optional* content, filled per municipality, and the hole they leave is not at
//! the edges but in the middle. Over the cell that covers the study quay in
//! Groningen the BGT registers **no trees at all**; the city's own register has
//! **3,018**. Two cells out, where the BGT does answer, it answers with 112 and
//! 230 against the city's 5,014 and 4,415. So this is not a gap to be filled at
//! the margin — outside its own coverage the national register is a rounding
//! error against the city's, and where a city register exists it is the one to
//! read.
//!
//! # One register per city, and the city decides how it is asked
//!
//! There is no national municipal API and there is not going to be one. Every
//! city publishes on whatever it already runs — a GeoServer here, an ArcGIS
//! feature service there — in its own coordinate system, with its own field
//! names and its own vocabulary for how tall a tree is. So a [`TreeRegister`]
//! owns **the whole question**: which ground it covers, how to phrase a request
//! for one cell, and how to read the answer back. Adding Utrecht is one more
//! entry in [`REGISTERS`] and two functions beside it; nothing else in this file
//! and nothing at all outside it needs to know.
//!
//! What the entries share is only the far end: every parser normalises to
//! [`RegisteredTree`], and that — not the city's own wire format — is what is
//! cached and what `map_geometry` plants. A city that answers in GML, or in RD
//! metres, or with heights in bands of Dutch prose, is that city's parser's
//! problem and stops there.
//!
//! # A city register replaces the national one, it does not add to it
//!
//! This is the opposite of the rule `systems::surveyed` follows against the
//! *scatter*, and for a reason that does not apply there. The scatter fills park
//! and wood polygons while a surveyed tree stands on a pavement, so the two
//! describe different trees and adding them is right. Two **registers**, on the
//! other hand, describe the same trees: the BGT's 112 in a cell are a subset of
//! the city's 5,014 in the same cell, so drawing both plants a second lime a
//! metre from the first, and the pair z-fight their own canopies. Matching them
//! up by distance would be arbitration — a tolerance to tune, wrong in both
//! directions — so the overlap is removed instead: over a covered cell the city
//! is the source and the BGT is not read. The scatter is untouched either way.

use crate::platform::{http, storage};
use crate::systems::height::{rd_to_wgs84, wgs84_to_rd};
use crate::systems::scatter::TreeInstance;
use cartopolis_core::world::tile_to_lat_lng;

/// The zoom cells are cut at — `map_geometry::GEOMETRY_ZOOM`, because these trees
/// are baked into that cell's merged meshes and have to arrive on the worker
/// that builds them.
const REGISTER_ZOOM: u32 = 14;

/// Cache-key generation. Bumping it retires every stored cell in one step, which
/// is the only way a fix reaches an existing install: nothing revalidates a
/// cached cell, the sweep is by size and not by age. Same device as
/// `surveyed::cell_cache_key`'s `v2`.
const CACHE_GENERATION: &str = "v2";

/// Most trees one cell may take from a register.
///
/// **Deliberately not `scatter::MAX_TREES_PER_CELL`**, which is the scatter's own,
/// and the
/// two caps bound different things. The scatter's bounds a *generated* lattice —
/// a cell that is all forest would emit ~35,000 trees and the number is a
/// decision, not a fact. This one bounds a **count of real trees**, so any cell
/// it actually cuts is a cell drawn wrong. Reusing 6,000 did exactly that on the
/// first run: two Groningen cells were truncated at 6,000 out of 6,263 and 6,808,
/// quietly losing 800 surveyed trees.
///
/// 12,000 is ~20× the municipal mean (126,060 trees over 447 km², against
/// 2.14 km² in a z14 cell here) and ~1.8× the densest cell measured. It is a
/// backstop against a register that answers with something absurd, not a
/// sampling rule — which is why hitting it is a logged warning.
const MAX_REGISTERED_TREES_PER_CELL: usize = 12_000;

/// One tree as its municipality registered it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegisteredTree {
    pub lat: f64,
    pub lng: f64,
    /// Height of the crown above the ground in metres, when the register says.
    ///
    /// `None` is ordinary, not an error — Groningen leaves it blank on about one
    /// tree in thirty — and means "draw it the way an unmeasured street tree has
    /// always been drawn", so a register with no heights at all still works and
    /// looks exactly like the BGT's trees do today.
    pub height_m: Option<f32>,
}

/// One municipality's own tree register: where it applies, how to ask it, and how
/// to read what it says.
pub struct TreeRegister {
    /// Short, stable, and part of the cache key — renaming it orphans a city's
    /// stored cells rather than serving them to the wrong parser.
    pub id: &'static str,
    /// The attribution line. Not decoration: these are published under CC BY,
    /// unlike the CC0 national registers around them, so the credit is a licence
    /// condition and the layer may not ship without showing it.
    pub attribution: &'static str,
    /// Coverage as `(south, west, north, east)` in WGS84 degrees.
    ///
    /// A box rather than the municipal boundary polygon, and deliberately: the
    /// cost of being too generous is one request that answers with nothing, and
    /// the cost of carrying 342 municipal boundaries to be exact is a dependency
    /// on a second dataset that also has to be kept current. Overlapping boxes
    /// are resolved by order in [`REGISTERS`].
    pub bounds: (f64, f64, f64, f64),
    /// The request for one cell, in whatever shape this city's service wants.
    pub url: fn(cx: i64, cy: i64) -> String,
    /// This city's answer, normalised.
    pub parse: fn(&str) -> Result<Vec<RegisteredTree>, String>,
}

/// Every city whose register this build can read.
///
/// One entry today. The list is the extension point the module note describes —
/// a second city is an entry here plus its own `url`/`parse` pair, and no caller
/// changes.
pub static REGISTERS: &[TreeRegister] = &[GRONINGEN];

/// Gemeente Groningen's tree register, over its own GeoServer.
///
/// Bounds are the layer's own published extent (RD 227,074–246,669 E by
/// 569,831–592,673 N) converted to degrees, so they are the municipality's
/// reach and not a guess at it.
const GRONINGEN: TreeRegister = TreeRegister {
    id: "groningen",
    attribution: "Street trees: Bomen gemeente Groningen (CC BY 4.0)",
    bounds: (53.1074, 6.4635, 53.3156, 6.7626),
    url: groningen_url,
    parse: parse_geoserver_rd_points,
};

/// Which register covers a cell, if any — the cell's **centre**, so a cell
/// straddling a city boundary belongs to exactly one register and cannot be
/// planted twice.
#[must_use]
pub fn register_for(cx: i64, cy: i64) -> Option<&'static TreeRegister> {
    let (north, west) = tile_to_lat_lng(cx, cy, REGISTER_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, REGISTER_ZOOM);
    let (lat, lng) = ((north + south) / 2.0, (west + east) / 2.0);
    REGISTERS.iter().find(|r| {
        (r.bounds.0..=r.bounds.2).contains(&lat) && (r.bounds.1..=r.bounds.3).contains(&lng)
    })
}

/// The attribution lines for every register that has actually planted something
/// this session.
///
/// Gated on use rather than listed statically, because a credit for a city the
/// player has never been near is noise in a panel that is already a wall of
/// them — and because the licence obliges attribution for data *used*.
#[must_use]
pub fn attributions() -> Vec<&'static str> {
    let used = USED.lock().map(|u| u.clone()).unwrap_or_default();
    REGISTERS
        .iter()
        .filter(|r| used.iter().any(|id| *id == r.id))
        .map(|r| r.attribution)
        .collect()
}

/// Registers that have answered with at least one tree. See [`attributions`].
static USED: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

/// One outage gate for the whole layer rather than one per city.
///
/// The other third-party layers here keep a breaker per service because theirs
/// can genuinely fail apart — RWS's marks and ProRail's crossings sit behind one
/// hostname and answer independently. These cannot overlap at all: a cell belongs
/// to exactly one register, so no two of them are ever tried for the same ground,
/// and a second city's cells would simply find the gate open. Split it the day
/// two registers are reachable for one cell, which is the day the coverage boxes
/// are allowed to overlap.
pub(crate) static REGISTER_BREAKER: http::Breaker = http::Breaker::new();

fn mark_used(id: &'static str) {
    if let Ok(mut u) = USED.lock() {
        if !u.contains(&id) {
            u.push(id);
        }
    }
}

/// Fetch and parse one cell's registered trees, through the storage cache.
///
/// Empty outside every register's coverage, and empty when the city's service is
/// unreachable — in both cases the caller's fallback is the layer it already had,
/// which is the scatter plus whatever the BGT holds.
///
/// **The cache stores the normalised form, not the city's answer.** Groningen's
/// GeoJSON is ~265 bytes a tree even trimmed to the two fields that are read; the
/// normalised form is ~30. Over a dense cell that is 1.3 MB against 150 kB, and
/// the fat one buys nothing — every field it carries was already discarded by the
/// parser that wrote it.
///
/// A failure is remembered on the **service**, not in the cache, exactly as
/// `crossings` and `nav_marks` do it against their own third parties and for the
/// same reason: `map_geometry` retears every cell in the ring on a re-anchor or a
/// layer toggle, and without the breaker each retear would re-pay the full wait
/// for every cell of a city that is down. Caching the failure instead is worse —
/// a poisoned key is never re-fetched, so an empty cell written under one is
/// permanent.
pub(crate) async fn load_registered_trees(cx: i64, cy: i64) -> Vec<RegisteredTree> {
    let Some(register) = register_for(cx, cy) else {
        return Vec::new();
    };
    let key = format!(
        "trees/{}/{CACHE_GENERATION}/{REGISTER_ZOOM}/{cx}/{cy}.json",
        register.id
    );
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        let trees = parse_normalised(&cached).unwrap_or_default();
        if !trees.is_empty() {
            mark_used(register.id);
        }
        return trees;
    }
    if REGISTER_BREAKER.is_open_at(web_time::Instant::now()) {
        return Vec::new();
    }
    // The measured worst case is 0.73 s for the densest Groningen cell at
    // 1.3 MB (2026-09-05, three runs), so the shared optional-layer bound is
    // four times the slowest answer this layer has ever given — and the point of
    // it is the *unresponsive* case, where the ground must not wait on a city.
    let request = http::Request::get((register.url)(cx, cy)).timeout(http::OPTIONAL_LAYER_TIMEOUT);
    let Ok(bytes) = http::fetch(request).await else {
        REGISTER_BREAKER.trip();
        return Vec::new();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        REGISTER_BREAKER.trip();
        return Vec::new();
    };
    let trees = match (register.parse)(&text) {
        Ok(trees) => trees,
        Err(e) => {
            tracing::warn!(register = register.id, cell = ?(cx, cy), error = %e,
                "municipal tree register did not parse");
            REGISTER_BREAKER.trip();
            return Vec::new();
        }
    };
    storage::put(
        storage::Namespace::Cache,
        &key,
        write_normalised(&trees).as_bytes(),
    )
    .await;
    if !trees.is_empty() {
        mark_used(register.id);
    }
    trees
}

/// This cell's registered trees as instances, ready to join the scattered ones.
///
/// Trees outside the cell are dropped here rather than in the request: a bounding
/// box in the city's coordinate system does not have the cell's edges in it (the
/// two grids are rotated against each other), so the request is deliberately a
/// little generous and containment is settled in the cell's own frame.
#[must_use]
pub fn registered_trees(
    trees: &[RegisteredTree],
    cx: i64,
    cy: i64,
    tile_m: f32,
    seed: u64,
) -> Vec<TreeInstance> {
    let points: Vec<(f32, f32, Option<f32>)> = trees
        .iter()
        .filter_map(|t| {
            crate::systems::surveyed::cell_local(t.lat, t.lng, cx, cy, REGISTER_ZOOM, tile_m)
                .map(|(x, z)| (x, z, t.height_m))
        })
        .take(MAX_REGISTERED_TREES_PER_CELL)
        .collect();
    crate::systems::scatter::surveyed_trees(&points, seed)
}

// ─── Groningen ───────────────────────────────────────────────────────────────

/// One cell of Groningen's register, as GeoJSON in RD metres.
///
/// Two query parameters are load-bearing. `propertyName` asks for the geometry
/// and the height band and nothing else — the layer carries fifteen fields and
/// the full record is 615 bytes a tree against 265 for these two. And `count` is
/// [`MAX_REGISTERED_TREES_PER_CELL`], so one pathological cell cannot hand the
/// streamer an unbounded download; a cell that hits it is logged by
/// [`parse_geoserver_rd_points`] rather than silently truncated.
fn groningen_url(cx: i64, cy: i64) -> String {
    let [x0, y0, x1, y1] = cell_rd_bbox(cx, cy);
    format!(
        "https://maps.groningen.nl/geoserver/wfs\
         ?service=WFS&version=2.0.0&request=GetFeature\
         &typeNames=geo-data%3ABomen%20gemeente%20Groningen\
         &outputFormat=application%2Fjson&propertyName=GEOM%2CBOOMHOOGTE\
         &count={MAX_REGISTERED_TREES_PER_CELL}\
         &bbox={x0:.1},{y0:.1},{x1:.1},{y1:.1},EPSG:28992"
    )
}

/// The RD box to ask for a cell, from its corners **and** its edge midpoints.
///
/// RD's grid is rotated against Web Mercator's by the meridian convergence, so
/// the extreme easting along a cell's north edge is not necessarily at a corner
/// and a box that only spans the corners can miss ground the cell will ask about.
/// `terrain::field_request` takes the same care for the same reason.
fn cell_rd_bbox(cx: i64, cy: i64) -> [f64; 4] {
    let (north, west) = tile_to_lat_lng(cx, cy, REGISTER_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, REGISTER_ZOOM);
    let mut min = [f64::MAX, f64::MAX];
    let mut max = [f64::MIN, f64::MIN];
    for lat in [south, (south + north) / 2.0, north] {
        for lng in [west, (west + east) / 2.0, east] {
            let rd = wgs84_to_rd(lat, lng);
            min = [min[0].min(rd[0]), min[1].min(rd[1])];
            max = [max[0].max(rd[0]), max[1].max(rd[1])];
        }
    }
    [min[0], min[1], max[0], max[1]]
}

/// A GeoServer GeoJSON page of RD points carrying a Dutch height band.
///
/// Shareable by any city on the same stack; the field names are the part that
/// would differ, and they are named here rather than in the struct so a second
/// GeoServer city can copy this function and change two strings.
fn parse_geoserver_rd_points(text: &str) -> Result<Vec<RegisteredTree>, String> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        features: Vec<Feature>,
        #[serde(rename = "numberReturned", default)]
        returned: usize,
        #[serde(rename = "numberMatched", default)]
        matched: usize,
    }
    #[derive(serde::Deserialize)]
    struct Feature {
        geometry: Option<Geometry>,
        #[serde(default)]
        properties: Props,
    }
    #[derive(serde::Deserialize)]
    struct Geometry {
        #[serde(default)]
        coordinates: Vec<f64>,
    }
    #[derive(serde::Deserialize, Default)]
    struct Props {
        #[serde(rename = "BOOMHOOGTE", default)]
        height_band: Option<String>,
    }

    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if wire.matched > wire.returned && wire.returned > 0 {
        // The ceiling is a guard against an unbounded download, not a sampling
        // rule, and GeoServer's page order is arbitrary — so a cell that hits it
        // loses trees in no particular pattern. Say so rather than let a thinned
        // avenue read as the register's own answer.
        tracing::warn!(
            returned = wire.returned,
            matched = wire.matched,
            "municipal tree register truncated a cell at the per-cell ceiling"
        );
    }
    // Exact duplicate positions are ghost *versions* of one tree, the same fault
    // `surveyed::parse_surveyed_cell` guards against: two identical canopies in
    // one place z-fight their own surfaces.
    let mut seen = std::collections::HashSet::new();
    Ok(wire
        .features
        .into_iter()
        .filter_map(|f| {
            let g = f.geometry?;
            let (&x, &y) = (g.coordinates.first()?, g.coordinates.get(1)?);
            seen.insert((x.to_bits(), y.to_bits())).then(|| {
                let (lat, lng) = rd_to_wgs84(x, y);
                RegisteredTree {
                    lat,
                    lng,
                    height_m: f.properties.height_band.as_deref().and_then(height_band_m),
                }
            })
        })
        .collect())
}

/// A Dutch height band — `"15 tot 18 m."`, `"tot 6 m."`, `"24 m. en hoger"` — as
/// one number in metres.
///
/// Read off the numbers in the string rather than matched against a table of the
/// eight spellings seen today, because the register is a city's operational
/// database and a ninth band costs nothing to add there. The two open bands are
/// the only judgement in here: the lowest is taken at three quarters of its
/// ceiling, since a tree small enough to sit at the bottom of it is a sapling the
/// register does not carry, and the highest a little above its floor.
fn height_band_m(band: &str) -> Option<f32> {
    let mut nums = band
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<f32>().ok());
    let first = nums.next()?;
    match nums.next() {
        Some(second) => Some((first + second) / 2.0),
        None if band.contains("hoger") => Some(first * 1.15),
        None => Some(first * 0.75),
    }
}

// ─── The normalised form ─────────────────────────────────────────────────────

/// `{"trees":[[lat,lng,height_or_null],…]}` — what the cache holds, and the only
/// shape anything outside a register's own parser ever sees.
fn write_normalised(trees: &[RegisteredTree]) -> String {
    let mut out = String::from("{\"trees\":[");
    for (i, t) in trees.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match t.height_m {
            Some(h) => out.push_str(&format!("[{:.7},{:.7},{h:.1}]", t.lat, t.lng)),
            None => out.push_str(&format!("[{:.7},{:.7},null]", t.lat, t.lng)),
        }
    }
    out.push_str("]}");
    out
}

fn parse_normalised(text: &str) -> Result<Vec<RegisteredTree>, String> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        trees: Vec<(f64, f64, Option<f32>)>,
    }
    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(wire
        .trees
        .into_iter()
        .map(|(lat, lng, height_m)| RegisteredTree { lat, lng, height_m })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cell that covers the study quay — the case this module exists for.
    const STUDY_CELL: (i64, i64) = (8491, 5320);

    #[test]
    fn groningen_covers_its_own_streets_and_not_utrecht() {
        assert_eq!(
            register_for(STUDY_CELL.0, STUDY_CELL.1).map(|r| r.id),
            Some("groningen")
        );
        // The cell over Utrecht (52.0907, 5.1214). No register yet, and the
        // lookup must say so rather than reach for the nearest one — a city with
        // no entry falls back to the BGT, which is the whole point of keeping
        // both.
        assert!(register_for(8425, 5405).is_none());
    }

    #[test]
    fn a_cells_request_box_is_in_rd_metres_and_contains_the_cell() {
        let [x0, y0, x1, y1] = cell_rd_bbox(STUDY_CELL.0, STUDY_CELL.1);
        // the study quay itself, from the register's own coordinates.
        let rd = wgs84_to_rd(53.225_114_0, 6.578_130_6);
        assert!(
            x0 < rd[0] && rd[0] < x1,
            "easting {} not in {x0}..{x1}",
            rd[0]
        );
        assert!(
            y0 < rd[1] && rd[1] < y1,
            "northing {} not in {y0}..{y1}",
            rd[1]
        );
        // A z14 cell up here is ~1.5 km across; a box wildly bigger than that
        // would be a projection mistake answering with half the city.
        assert!((x1 - x0) < 2_500.0 && (y1 - y0) < 2_500.0);
    }

    /// Every band the register uses today, plus the two open ones, which are the
    /// only places this function decides anything.
    /// Two registers claiming one piece of ground is the case the whole design
    /// avoids: `register_for` would pick by list order, the loser's city would be
    /// silently unreachable, and the single [`REGISTER_BREAKER`] would gate a
    /// service that was never the failing one. Cheap to assert while the list is
    /// short, and this is the moment it stops being obvious.
    #[test]
    fn no_two_registers_claim_the_same_ground() {
        for (i, a) in REGISTERS.iter().enumerate() {
            for b in &REGISTERS[i + 1..] {
                let lat_overlap = a.bounds.0 <= b.bounds.2 && b.bounds.0 <= a.bounds.2;
                let lng_overlap = a.bounds.1 <= b.bounds.3 && b.bounds.1 <= a.bounds.3;
                assert!(
                    !(lat_overlap && lng_overlap),
                    "{} and {} claim overlapping ground",
                    a.id,
                    b.id
                );
            }
        }
    }

    #[test]
    fn height_bands_read_as_metres() {
        assert_eq!(height_band_m("6 tot 9 m."), Some(7.5));
        assert_eq!(height_band_m("15 tot 18 m."), Some(16.5));
        assert_eq!(height_band_m("18 tot 24 m."), Some(21.0));
        assert_eq!(height_band_m("tot 6 m."), Some(4.5));
        assert_eq!(height_band_m("24 m. en hoger"), Some(27.599_998));
        assert_eq!(height_band_m(""), None);
        assert_eq!(height_band_m("onbekend"), None);
    }

    /// A real page shape, trimmed: RD point geometry, a band, a null band, and a
    /// duplicate of the first tree.
    #[test]
    fn a_geoserver_page_parses_into_normalised_trees() {
        let page = r#"{"type":"FeatureCollection","numberReturned":3,"numberMatched":3,
            "features":[
              {"type":"Feature","geometry":{"type":"Point","coordinates":[234729.772,582447.843]},
               "properties":{"BOOMHOOGTE":"15 tot 18 m."}},
              {"type":"Feature","geometry":{"type":"Point","coordinates":[234800.0,582500.0]},
               "properties":{"BOOMHOOGTE":null}},
              {"type":"Feature","geometry":{"type":"Point","coordinates":[234729.772,582447.843]},
               "properties":{"BOOMHOOGTE":"15 tot 18 m."}}]}"#;
        let trees = parse_geoserver_rd_points(page).expect("a valid page");
        assert_eq!(trees.len(), 2, "the repeated position is one tree");
        assert_eq!(trees[0].height_m, Some(16.5));
        assert_eq!(trees[1].height_m, None);
        // The register publishes this tree's own longitude and latitude beside
        // the geometry; the conversion has to land on them.
        assert!(
            (trees[0].lat - 53.222_694_63).abs() < 1e-5,
            "lat {}",
            trees[0].lat
        );
        assert!(
            (trees[0].lng - 6.580_966_22).abs() < 1e-5,
            "lng {}",
            trees[0].lng
        );
    }

    /// The cache round trip. A height that survives as `None` is the case that
    /// would otherwise turn every unmeasured tree into a zero-metre one.
    #[test]
    fn the_normalised_form_round_trips() {
        let trees = vec![
            RegisteredTree {
                lat: 53.222_694_6,
                lng: 6.580_966_2,
                height_m: Some(16.5),
            },
            RegisteredTree {
                lat: 53.225_114_0,
                lng: 6.578_130_6,
                height_m: None,
            },
        ];
        let back = parse_normalised(&write_normalised(&trees)).expect("round trip");
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].height_m, Some(16.5));
        assert_eq!(back[1].height_m, None);
        assert!((back[0].lat - trees[0].lat).abs() < 1e-6);
        assert!((back[1].lng - trees[1].lng).abs() < 1e-6);
    }

    #[test]
    fn a_page_that_is_not_json_is_an_error_not_an_empty_cell() {
        assert!(parse_geoserver_rd_points("<ows:ExceptionReport/>").is_err());
    }
}
