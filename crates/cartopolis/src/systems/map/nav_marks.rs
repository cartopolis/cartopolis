//! The streaming half of the navigation marks: the two PDOK collections, the
//! per-cell cache, and the projection into the frame the meshes are built in.
//!
//! The pure half — what a mark is, what its colours mean and how a cell's marks
//! become meshes — is `cartopolis_geo::nav_marks`, and
//! `docs/notes/navigation-marks.md` has the endpoint, the licence and the
//! measurements behind both.
//!
//! There is no streamer here and no resource, for the same reason
//! `systems::surveyed` and `systems::crops` have none: a mark ends up merged into
//! its cell's furniture mesh, so it has to be in hand on the worker that builds
//! it rather than arriving later on a schedule of its own. `map_geometry` awaits
//! it beside the surveyed paving and the crop parcels.
//!
//! # Why the client fetches this itself
//!
//! The other four Dutch registers are extracted by cartopy and served per cell by
//! our own host (`docs/notes/where-the-map-data-is-built.md`), and that seam is
//! worth its cost for a 2 MB-per-cell dataset that needs reprojection and
//! triangulation. It is not worth it for 18,474 points behind a keyless `bbox`
//! query that answers `Access-Control-Allow-Origin: *` with an hour of cache
//! control on it. The in-tree precedent is `systems::height` and
//! `systems::aerial`, which reach PDOK directly, and `systems::tall_structures`,
//! which streams a third party per cell.
//!
//! There is no [`CoverageSlot`](crate::systems::coverage::CoverageSlot) either.
//! That answers "which cells does *our host* serve", which is not a question
//! here — coverage is an **envelope**, exactly as `aerial` decided for the same
//! service and the same country.
//!
//! # Two gates before a request is made
//!
//! Most of the planet is neither Dutch nor wet, and an empty answer still costs a
//! round trip and ~1.9 KB (measured over Groningen's Grote Markt, which is inside
//! the envelope and has no water in the cell).
//!
//! 1. **The collections' own spatial extent.** Same rule `aerial` states:
//!    coverage is an envelope, never a status code.
//! 2. **The cell's water.** A mark stands in a fairway, and a cell with no water
//!    layer in its tile cannot hold one. The tile body is in hand at this point
//!    but *not parsed* — the decode belongs on a worker (ADR-033) and the fetch
//!    decision has to be made before it — so this reads the undecoded body
//!    through [`vector_tiles::body_mentions_water`], which cannot answer "dry"
//!    for a body that has a water layer in it.
//!
//! The Layers panel's Furniture toggle is a third gate, applied by the caller:
//! marks ride that group's mesh, its material and its altitude fade, so a run
//! with furniture off asks for nothing at all.
//!
//! # …and a fourth, once the service has failed
//!
//! Those three are about the cell. The fourth is about the *service*: this fetch
//! sits between the cell's tile body and its tessellation worker, so a slow
//! `api.pdok.nl` costs the ground, the roads and the buildings rather than the
//! buoys. Two bounds, both in `platform::http`. Every request here carries
//! [`http::OPTIONAL_LAYER_TIMEOUT`] instead of the 20 s a fetcher inherits by
//! not asking, and a failure trips [`MARKS_BREAKER`], which holds the service
//! off for a minute — because nothing is cached on failure (see
//! [`load_marks_cell`]), and `map_geometry` re-runs the whole task on every
//! retear, so without it each re-anchor and each layer toggle re-pays the wait
//! for every cell in the ring.

use cartopolis_geo::nav_marks::{
    ColourPattern, FixedForm, MarkColour, MarkForm, MarkKind, MarkShape, NavMark, Topmark,
    MAX_MARKS_PER_CELL,
};

use crate::platform::{http, storage};
use crate::systems::surveyed::cell_local_unclamped;

/// Fixed cache/streaming zoom — must match `map_geometry::GEOMETRY_ZOOM`, since a
/// mark is meshed into that cell.
const MARK_ZOOM: u32 = 14;

/// PDOK's OGC API host. Unlike AHN this needs no same-origin relay on the web:
/// the service answers `Access-Control-Allow-Origin: *` (checked 2026-08-30),
/// the same reason `systems::aerial` reaches its WMTS directly.
const MARKS_ORIGIN: &str = "https://api.pdok.nl";

/// The service's entry point. Note the path: `…/rws/vaarwegmarkeringen/…`, which
/// is what the register's own name suggests, is a 404.
const MARKS_PATH: &str = "/rws/vaarwegmarkeringen-nederland/ogc/v1";

/// The two collections: floating marks (buoys) and fixed ones (beacons, groyne
/// marks, shore lights).
const COLLECTIONS: [&str; 2] = [
    "vaarweg_markeringen_drijvend_rd",
    "vaarweg_markeringen_vast_rd",
];

/// The service caps `limit` at 1000 and refuses anything larger. A z14 cell holds
/// tens of marks at most — the densest measured is 43 — so a page that comes back
/// exactly full means the cursor has more and something is very wrong.
const PAGE_LIMIT: usize = 1000;

/// The collections' own `extent.spatial.bbox`, from the collections document
/// (CRS84). It reaches past the coast because the register covers the North Sea
/// approaches.
const MARKS_LON: (f64, f64) = (2.35, 7.56);
const MARKS_LAT: (f64, f64) = (50.71, 55.67);

/// The register's two "nothing here" sentinels. `#` is the third, used on
/// `opgeheven`.
fn is_absent(value: &str) -> bool {
    matches!(value.trim(), "" | "X" | "#" | "Niet toegewezen")
}

// ─── The cached form ─────────────────────────────────────────────────────────

/// The forms, in cache order. **Append, never renumber** — and renumbering means
/// bumping the `v1` in [`cache_key`], because a cached cell written by an older
/// table would otherwise be read back as the wrong marks.
const KINDS: [MarkKind; 8] = [
    MarkKind::Floating(MarkShape::Spar),
    MarkKind::Floating(MarkShape::Can),
    MarkKind::Floating(MarkShape::Conical),
    MarkKind::Floating(MarkShape::Spherical),
    MarkKind::Floating(MarkShape::Pillar),
    MarkKind::Fixed(FixedForm::GroyneMark),
    MarkKind::Fixed(FixedForm::Beacon),
    MarkKind::Fixed(FixedForm::LightPost),
];

/// The colours, in cache order. Same rule as [`KINDS`].
const COLOURS: [MarkColour; 7] = [
    MarkColour::Red,
    MarkColour::Green,
    MarkColour::Yellow,
    MarkColour::White,
    MarkColour::Black,
    MarkColour::Grey,
    MarkColour::Amber,
];

/// The topmarks, in cache order. Same rule as [`KINDS`].
const TOPMARKS: [Topmark; 11] = [
    Topmark::Cylinder,
    Topmark::ConeUp,
    Topmark::ConeDown,
    Topmark::Sphere,
    Topmark::Cross,
    Topmark::TwoConesUp,
    Topmark::TwoConesDown,
    Topmark::TwoConesInward,
    Topmark::TwoConesOutward,
    Topmark::ConeOverSphere,
    Topmark::CylinderOverSphere,
];

/// One mark as it is cached: the position, and indices into the tables above.
///
/// **Not the GeoJSON.** A fixed feature carries ninety properties and runs to
/// ~2 KB, and `properties=` subsetting is a 400 on this service, so the Waal cell
/// is 57 KB on the wire and 4 KB here. Everything this client can draw is in
/// these seven fields; the rest — the light's character and period, the fairway
/// name, the RD coordinates, the sixteen sector definitions on every fixed
/// object — is dropped at parse time rather than carried through the cache.
#[derive(serde::Serialize, serde::Deserialize)]
struct WireMark {
    lat: f64,
    lng: f64,
    /// Index into [`KINDS`].
    k: usize,
    /// Body colour band indices into [`COLOURS`], top to bottom.
    b: Vec<usize>,
    /// The body pattern repeats (`… repeterend`).
    #[serde(default)]
    br: bool,
    /// Index into [`TOPMARKS`].
    #[serde(default)]
    t: Option<usize>,
    /// Topmark colour bands.
    #[serde(default)]
    tc: Vec<usize>,
    #[serde(default)]
    tr: bool,
    /// The light's colour, index into [`COLOURS`].
    #[serde(default)]
    l: Option<usize>,
}

/// One cell's marks, still in lat/lng as the register holds them.
#[derive(Clone, Debug, Default)]
pub struct MarkCell {
    pub marks: Vec<(f64, f64, MarkForm)>,
}

fn colour_index(colour: MarkColour) -> Option<usize> {
    COLOURS.iter().position(|c| *c == colour)
}

fn pattern_to_wire(pattern: &ColourPattern) -> (Vec<usize>, bool) {
    (
        pattern
            .bands
            .iter()
            .filter_map(|c| colour_index(*c))
            .collect(),
        pattern.repeating,
    )
}

fn pattern_from_wire(bands: &[usize], repeating: bool) -> ColourPattern {
    ColourPattern {
        bands: bands
            .iter()
            .filter_map(|i| COLOURS.get(*i).copied())
            .collect(),
        repeating,
    }
}

/// Serialise a cell for the cache.
fn encode_cell(cell: &MarkCell) -> String {
    let marks: Vec<WireMark> = cell
        .marks
        .iter()
        .filter_map(|(lat, lng, form)| {
            let k = KINDS.iter().position(|k| *k == form.kind)?;
            let (b, br) = pattern_to_wire(&form.body);
            let (t, tc, tr) = match &form.topmark {
                Some((topmark, colours)) => {
                    let (tc, tr) = pattern_to_wire(colours);
                    (TOPMARKS.iter().position(|x| x == topmark), tc, tr)
                }
                None => (None, Vec::new(), false),
            };
            Some(WireMark {
                lat: *lat,
                lng: *lng,
                k,
                b,
                br,
                t,
                tc,
                tr,
                l: form.light.and_then(colour_index),
            })
        })
        .collect();
    serde_json::json!({ "marks": marks }).to_string()
}

/// Read a cell back out of the cache.
///
/// An index this build has no form for drops the mark rather than clamping it
/// onto the last known one — the same rule `surveyed`'s `PROP_KINDS` and `crops`'
/// `CATEGORIES` follow, and the same reason: a mark drawn as the wrong shape or
/// the wrong colour says the wrong thing.
pub fn decode_cell(text: &str) -> Result<MarkCell, String> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        marks: Vec<WireMark>,
    }
    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(MarkCell {
        marks: wire
            .marks
            .into_iter()
            .filter_map(|m| {
                let kind = *KINDS.get(m.k)?;
                let body = pattern_from_wire(&m.b, m.br);
                if body.is_empty() {
                    return None;
                }
                let topmark = m.t.and_then(|i| TOPMARKS.get(i).copied()).and_then(|t| {
                    let colours = pattern_from_wire(&m.tc, m.tr);
                    (!colours.is_empty()).then_some((t, colours))
                });
                Some((
                    m.lat,
                    m.lng,
                    MarkForm {
                        kind,
                        body,
                        topmark,
                        light: m.l.and_then(|i| COLOURS.get(i).copied()),
                    },
                ))
            })
            .collect(),
    })
}

// ─── The register ────────────────────────────────────────────────────────────

/// Does the register cover this cell at all?
///
/// Tested on the cell's own corners rather than its centre, so a coastal cell
/// half inside the envelope is still asked about.
#[must_use]
pub fn covers(cx: i64, cy: i64) -> bool {
    let (north, west) = cartopolis_core::world::tile_to_lat_lng(cx, cy, MARK_ZOOM);
    let (south, east) = cartopolis_core::world::tile_to_lat_lng(cx + 1, cy + 1, MARK_ZOOM);
    west <= MARKS_LON.1 && east >= MARKS_LON.0 && south <= MARKS_LAT.1 && north >= MARKS_LAT.0
}

/// The cell's own two gates: is this cell one the register could have marks in?
///
/// True of the cell forever, so this is also what decides whether the cache is
/// consulted at all — a foreign or dry cell must cost neither a request nor a
/// storage lookup.
fn cell_may_hold_marks(cx: i64, cy: i64, tile_has_water: bool) -> bool {
    covers(cx, cy) && tile_has_water
}

/// Every gate, as one predicate: is this cell worth a *request*?
///
/// The cell's own two, plus the service's outage gate. Split out of
/// [`load_marks_cell`] so the rule can be asserted without a runtime and without
/// reaching the network — which is the only way to test "no request was issued".
/// `breaker` and `now` are injected for the same reason.
fn should_fetch(
    cx: i64,
    cy: i64,
    tile_has_water: bool,
    breaker: &http::Breaker,
    now: web_time::Instant,
) -> bool {
    cell_may_hold_marks(cx, cy, tile_has_water) && !breaker.is_open_at(now)
}

/// RWS's marks service, held off for a minute after it fails. Its own instance:
/// ProRail's crossings service sits behind the same host and fails separately.
pub(crate) static MARKS_BREAKER: http::Breaker = http::Breaker::new();

fn cache_key(cx: i64, cy: i64) -> String {
    format!("features/marks/v1/{MARK_ZOOM}/{cx}/{cy}.json")
}

/// One collection's `items` answer, on the optional-layer bound rather than the
/// 20 s a fetcher inherits by not asking.
async fn fetch_collection(collection: &str, cx: i64, cy: i64) -> Result<String, String> {
    let url = items_url(collection, cx, cy);
    http::fetch_string(http::Request::get(url).timeout(http::OPTIONAL_LAYER_TIMEOUT)).await
}

fn items_url(collection: &str, cx: i64, cy: i64) -> String {
    let (north, west) = cartopolis_core::world::tile_to_lat_lng(cx, cy, MARK_ZOOM);
    let (south, east) = cartopolis_core::world::tile_to_lat_lng(cx + 1, cy + 1, MARK_ZOOM);
    format!(
        "{MARKS_ORIGIN}{MARKS_PATH}/collections/{collection}/items\
         ?f=json&limit={PAGE_LIMIT}&bbox={west},{south},{east},{north}"
    )
}

/// Pull one property as a string. The register writes numbers for a few of them,
/// so a `Value::String`-only read would silently drop those — the same trap the
/// `pois` house numbers had.
fn prop<'a>(feature: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    feature.get("properties")?.get(key)?.as_str()
}

/// The first coordinate of a feature's geometry.
///
/// Every feature in both collections is a **`MultiPoint` with one coordinate**,
/// not a `Point` — a reader that matches only `Point` finds nothing at all and
/// looks exactly like a region with no marks in it. `Point` is accepted too, in
/// case the service ever simplifies.
fn position(feature: &serde_json::Value) -> Option<(f64, f64)> {
    let geometry = feature.get("geometry")?;
    let coords = geometry.get("coordinates")?;
    let pair = match geometry.get("type")?.as_str()? {
        "Point" => coords,
        "MultiPoint" => coords.get(0)?,
        _ => return None,
    };
    // CRS84: longitude first. The collections are named `_rd` and store EPSG:28992,
    // but the default output CRS is CRS84 and that is what is asked for.
    let lng = pair.get(0)?.as_f64()?;
    let lat = pair.get(1)?.as_f64()?;
    Some((lat, lng))
}

/// Turn one GeoJSON feature into a mark, or drop it.
fn read_feature(feature: &serde_json::Value, floating: bool) -> Option<(f64, f64, MarkForm)> {
    // A decommissioned mark is not there any more. Unlike the BGT this API is not
    // returning historical versions — every one of 1,258 sampled features carries
    // `#` — but the field exists and the register is entitled to start using it.
    if !prop(feature, "opgeheven").is_none_or(is_absent) {
        return None;
    }
    let kind = if floating {
        MarkKind::Floating(MarkShape::parse(prop(feature, "obj_vorm")?)?)
    } else {
        MarkKind::Fixed(FixedForm::parse(prop(feature, "naut_funct")?)?)
    };
    let body = ColourPattern::parse(prop(feature, "obj_kleur").unwrap_or_default());
    if body.is_empty() {
        return None;
    }
    // Two keys for one vocabulary: the floating collection writes `tt_toptek`
    // and the fixed one `v_toptek`. Reading only the first draws every river
    // beacon without the cone or cylinder that says which bank it marks.
    let topmark = prop(feature, "tt_toptek")
        .or_else(|| prop(feature, "v_toptek"))
        .and_then(Topmark::parse)
        .and_then(|t| {
            let colours = ColourPattern::parse(prop(feature, "tt_kleur").unwrap_or_default());
            (!colours.is_empty()).then_some((t, colours))
        });
    // …and two keys for the light colour, on the same terms.
    let light = prop(feature, "licht_klr")
        .or_else(|| prop(feature, "licht_kl"))
        .and_then(MarkColour::parse);
    let (lat, lng) = position(feature)?;
    Some((
        lat,
        lng,
        MarkForm {
            kind,
            body,
            topmark,
            light,
        },
    ))
}

/// Parse one collection's `items` response.
pub fn parse_items(text: &str, floating: bool) -> Result<Vec<(f64, f64, MarkForm)>, String> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let features = json
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| "no features in the marks response".to_string())?;
    // A full page means the cursor has more. At z14 it never should — the densest
    // cell in the country is 43 marks — so this is a loud line rather than a
    // silent second page.
    if features.len() >= PAGE_LIMIT {
        tracing::warn!(
            limit = PAGE_LIMIT,
            "navigation marks came back a full page; this cell is marked short"
        );
    }
    Ok(features
        .iter()
        .filter_map(|f| read_feature(f, floating))
        .collect())
}

/// Fetch and parse one cell's marks, through the storage cache.
///
/// `tile_has_water` is the second gate: see the module note for why it reads an
/// undecoded body rather than a parse. An ungated cell, an unreachable service
/// and an empty answer all produce the same empty cell, which is the water this
/// client already drew.
pub(crate) async fn load_marks_cell(cx: i64, cy: i64, tile_has_water: bool) -> MarkCell {
    if !cell_may_hold_marks(cx, cy, tile_has_water) {
        return MarkCell::default();
    }
    let key = cache_key(cx, cy);
    // The cache is read before the breaker is consulted, not after: a cell whose
    // marks are already on disk costs the service nothing, and an outage must
    // not blank cells that were complete before it started.
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        return decode_cell(&cached).unwrap_or_default();
    }
    if !should_fetch(
        cx,
        cy,
        tile_has_water,
        &MARKS_BREAKER,
        web_time::Instant::now(),
    ) {
        return MarkCell::default();
    }

    // Both collections in flight together rather than one after the other, so a
    // cell's wait is one service's answer and not two of them end to end. On
    // wasm that is real overlap; natively `http::fetch` is a blocking `ureq`
    // call with no await point in it (`platform::http`'s module note), so the
    // two still land in sequence there and the bound below is what caps the
    // total.
    let (floating, fixed) = futures_lite::future::zip(
        fetch_collection(COLLECTIONS[0], cx, cy),
        fetch_collection(COLLECTIONS[1], cx, cy),
    )
    .await;

    let mut marks = Vec::new();
    for (i, answer) in [floating, fixed].into_iter().enumerate() {
        // Nothing is written to the cache on a partial answer: half a cell
        // cached under a key that will never be re-fetched is worse than a cell
        // that is asked for again. What *is* remembered is that the service
        // failed, which is the breaker rather than the cache.
        let Ok(text) = answer else {
            MARKS_BREAKER.trip();
            return MarkCell::default();
        };
        match parse_items(&text, i == 0) {
            Ok(read) => marks.extend(read),
            Err(e) => {
                tracing::warn!(collection = COLLECTIONS[i], error = %e, "navigation marks unreadable");
                MARKS_BREAKER.trip();
                return MarkCell::default();
            }
        }
    }
    if marks.len() > MAX_MARKS_PER_CELL {
        tracing::warn!(
            cap = MAX_MARKS_PER_CELL,
            found = marks.len(),
            "navigation marks hit the per-cell cap; this cell is marked short"
        );
        marks.truncate(MAX_MARKS_PER_CELL);
    }

    let cell = MarkCell { marks };
    storage::put(
        storage::Namespace::Cache,
        &key,
        encode_cell(&cell).as_bytes(),
    )
    .await;
    cell
}

/// This cell's marks in the local metre frame, ready to mesh.
///
/// A mark outside the cell is dropped rather than drawn: the bbox query is the
/// cell's own, so this only catches the boundary case, and unlike a crop parcel a
/// mark is a point that belongs to exactly one cell — drawing it from two would
/// stand two buoys on one spot.
#[must_use]
pub fn cell_marks(cell: &MarkCell, cx: i64, cy: i64, tile_m: f32) -> Vec<NavMark> {
    cell.marks
        .iter()
        .filter_map(|(lat, lng, form)| {
            let (x, z) = cell_local_unclamped(*lat, *lng, cx, cy, MARK_ZOOM, tile_m);
            (x >= 0.0 && z >= 0.0 && x < tile_m && z < tile_m).then(|| NavMark {
                x,
                z,
                form: form.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::world::tile_to_lat_lng;

    /// Two real answers from the service, captured verbatim over the Waal cell on
    /// 2026-08-30 and trimmed to one feature each.
    ///
    /// Every hand-written fixture in this module is my idea of the shape; these
    /// two are the register's. They are what pins the three things that are
    /// invisible when they are wrong — the geometry is a `MultiPoint` and not a
    /// `Point`, the fixed collection writes `v_toptek` where the floating one
    /// writes `tt_toptek`, and `licht_kl` where it writes `licht_klr` — each of
    /// which fails by producing nothing at all, which is indistinguishable from
    /// water that has no marks in it.
    ///
    /// Ordinary text files, deliberately: `.gitattributes` does not track
    /// `.json`, so these are not LFS objects and cannot arrive as pointer files
    /// (CLAUDE.md's standing trap).
    const REAL_FLOATING: &str = include_str!("../../../tests/fixtures/nav_marks_floating.json");
    const REAL_FIXED: &str = include_str!("../../../tests/fixtures/nav_marks_fixed.json");

    /// The Waal at Nijmegen — the densest z14 cell measured, and the one the
    /// headless gate flies to.
    const WAAL: (i64, i64) = (8458, 5422);
    /// Groningen's Grote Markt: inside the envelope, no water in the cell, and
    /// zero marks in the register.
    const GRONINGEN: (i64, i64) = (8490, 5320);

    fn a_floating_feature(lat: f64, lng: f64) -> String {
        format!(
            r##"{{"type":"Feature",
                "properties":{{"obj_vorm":"stomp","obj_kleur":"Rood","opgeheven":"#",
                               "tt_toptek":"Cilinder","tt_kleur":"Rood","licht_klr":"Rood"}},
                "geometry":{{"type":"MultiPoint","coordinates":[[{lng},{lat}]]}}}}"##
        )
    }

    fn middle_of(cx: i64, cy: i64) -> (f64, f64) {
        let (north, west) = tile_to_lat_lng(cx, cy, MARK_ZOOM);
        let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, MARK_ZOOM);
        ((north + south) / 2.0, (west + east) / 2.0)
    }

    /// The two gates that keep this layer from asking the register about the
    /// rest of the planet, and about the dry two-thirds of the Netherlands.
    ///
    /// This is the whole cost argument for fetching from the client rather than
    /// through an extractor: without them every streamed cell anywhere in the
    /// world would pay two round trips to be told no.
    #[test]
    fn a_foreign_or_dry_cell_asks_for_nothing() {
        let (breaker, now) = (http::Breaker::new(), web_time::Instant::now());
        // Outside the envelope: a z14 cell over Warsaw, and one mid-Atlantic.
        // Neither asks, however much water the tile carries.
        for (cx, cy) in [(9130_i64, 5350_i64), (7000, 5400)] {
            assert!(!covers(cx, cy), "({cx}, {cy}) is not in the register");
            assert!(!should_fetch(cx, cy, true, &breaker, now));
            assert!(!cell_may_hold_marks(cx, cy, true));
        }
        // Inside the envelope, and wet: the one case that asks.
        assert!(covers(WAAL.0, WAAL.1));
        assert!(should_fetch(WAAL.0, WAAL.1, true, &breaker, now));
        // Inside the envelope and dry — Groningen's Grote Markt is in the
        // register's bbox and has no water in its cell, and the register has no
        // marks there. It must cost nothing rather than two empty answers.
        assert!(covers(GRONINGEN.0, GRONINGEN.1));
        assert!(!should_fetch(
            GRONINGEN.0,
            GRONINGEN.1,
            false,
            &breaker,
            now
        ));
        assert!(!cell_may_hold_marks(GRONINGEN.0, GRONINGEN.1, false));
    }

    /// The fourth gate: once the service has failed, the cell that would
    /// otherwise ask is refused for a minute — and then asks again.
    ///
    /// Asserted on the predicate with an injected breaker and an injected
    /// instant, because "no request was issued" is not observable from the
    /// other side of an `async fn` that returns the same empty cell either way,
    /// and because a test that reached for the wall clock would have to sleep a
    /// minute to see the far side of the cooldown.
    #[test]
    fn a_failed_service_is_not_asked_again_until_the_cooldown_passes() {
        let breaker = http::Breaker::new();
        let now = web_time::Instant::now();
        assert!(should_fetch(WAAL.0, WAAL.1, true, &breaker, now));

        breaker.trip_at(now);
        assert!(
            !should_fetch(WAAL.0, WAAL.1, true, &breaker, now),
            "a cell was asked about while the service was known to be down"
        );
        assert!(!should_fetch(
            WAAL.0,
            WAAL.1,
            true,
            &breaker,
            now + http::BREAKER_COOLDOWN - std::time::Duration::from_secs(1)
        ));
        assert!(
            should_fetch(WAAL.0, WAAL.1, true, &breaker, now + http::BREAKER_COOLDOWN),
            "the breaker never reopened"
        );
        // The cell's own gates are untouched by it: an outage does not make a
        // dry cell worth asking about once the service is back.
        assert!(cell_may_hold_marks(WAAL.0, WAAL.1, true));
        assert!(!cell_may_hold_marks(GRONINGEN.0, GRONINGEN.1, false));
    }

    /// The two ways the register says "nothing here". Getting either wrong is
    /// silent: `X` read as a colour paints a mark that means nothing, and
    /// `opgeheven` ignored draws marks that were lifted years ago.
    #[test]
    fn the_registers_sentinels_are_recognised() {
        for raw in ["X", "#", "Niet toegewezen", "  ", ""] {
            assert!(is_absent(raw), "{raw:?} read as a value");
        }
        assert!(!is_absent("Rood"));
    }

    #[test]
    fn a_multipoint_feature_is_read() {
        let (lat, lng) = middle_of(WAAL.0, WAAL.1);
        let text = format!(r#"{{"features":[{}]}}"#, a_floating_feature(lat, lng));
        let marks = parse_items(&text, true).expect("valid items");
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].2.kind, MarkKind::Floating(MarkShape::Can));
        assert_eq!(marks[0].2.body.bands, vec![MarkColour::Red]);
        assert_eq!(marks[0].2.light, Some(MarkColour::Red));
        assert!(marks[0].2.topmark.is_some());
        // Longitude first in CRS84 — swapping them puts every buoy in the sea
        // off Somalia.
        assert!((marks[0].0 - lat).abs() < 1e-9);
        assert!((marks[0].1 - lng).abs() < 1e-9);
    }

    /// The fixed collection writes the topmark under a different key. Reading
    /// only the floating one leaves every river beacon bare, which looks exactly
    /// like a register that records no topmarks.
    #[test]
    fn a_fixed_marks_topmark_comes_from_its_own_key() {
        let (lat, lng) = middle_of(WAAL.0, WAAL.1);
        let text = format!(
            r##"{{"features":[{{"type":"Feature",
                "properties":{{"naut_funct":"Kribbaken","obj_kleur":"Wit","opgeheven":"#",
                               "v_toptek":"Kegel, punt naar boven","tt_kleur":"Groen",
                               "licht_kl":"Niet toegewezen"}},
                "geometry":{{"type":"MultiPoint","coordinates":[[{lng},{lat}]]}}}}]}}"##
        );
        let marks = parse_items(&text, false).expect("valid items");
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].2.kind, MarkKind::Fixed(FixedForm::GroyneMark));
        assert_eq!(
            marks[0].2.topmark.as_ref().map(|(t, _)| *t),
            Some(Topmark::ConeUp)
        );
        assert_eq!(marks[0].2.light, None, "an unassigned light is no light");
    }

    #[test]
    fn a_decommissioned_mark_is_dropped() {
        let (lat, lng) = middle_of(WAAL.0, WAAL.1);
        let text = format!(
            r#"{{"features":[{{"type":"Feature",
                "properties":{{"obj_vorm":"spar","obj_kleur":"Groen","opgeheven":"01-01-2020"}},
                "geometry":{{"type":"MultiPoint","coordinates":[[{lng},{lat}]]}}}}]}}"#
        );
        assert!(parse_items(&text, true).expect("valid items").is_empty());
    }

    /// A form or a colour this build cannot name is dropped, never substituted.
    #[test]
    fn an_unknown_form_is_dropped_rather_than_guessed() {
        let (lat, lng) = middle_of(WAAL.0, WAAL.1);
        for props in [
            r##""obj_vorm":"Toren","obj_kleur":"Rood","opgeheven":"#""##,
            r##""obj_vorm":"spar","obj_kleur":"Niet toegewezen","opgeheven":"#""##,
        ] {
            let text = format!(
                r#"{{"features":[{{"type":"Feature","properties":{{{props}}},
                    "geometry":{{"type":"MultiPoint","coordinates":[[{lng},{lat}]]}}}}]}}"#
            );
            assert!(parse_items(&text, true).expect("valid items").is_empty());
        }
    }

    /// The cache is the only reader of this encoding, and a round trip has to be
    /// lossless for everything this client draws — an index table that drifted
    /// would redraw a red buoy green from cache with nothing to say it had.
    #[test]
    fn the_cached_form_round_trips() {
        let cell = MarkCell {
            marks: vec![
                (
                    51.85,
                    5.86,
                    MarkForm {
                        kind: MarkKind::Floating(MarkShape::Spar),
                        body: ColourPattern::parse("Rood/wit repeterend"),
                        topmark: Some((Topmark::Cross, ColourPattern::parse("Zwart"))),
                        light: Some(MarkColour::Yellow),
                    },
                ),
                (
                    51.86,
                    5.87,
                    MarkForm {
                        kind: MarkKind::Fixed(FixedForm::LightPost),
                        body: ColourPattern::parse("Wit"),
                        topmark: None,
                        light: None,
                    },
                ),
            ],
        };
        let back = decode_cell(&encode_cell(&cell)).expect("round trip");
        assert_eq!(back.marks.len(), 2);
        for (a, b) in cell.marks.iter().zip(&back.marks) {
            assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
            assert_eq!(a.2, b.2);
        }
    }

    /// A cache entry written by a newer build names a form this one has no shape
    /// for. Dropping it beats drawing whatever sits at the end of the table.
    #[test]
    fn an_out_of_range_cache_index_drops_its_mark() {
        let cell = decode_cell(r#"{"marks":[{"lat":51.8,"lng":5.8,"k":99,"b":[0]}]}"#)
            .expect("valid cache entry");
        assert!(cell.marks.is_empty());
        // …and a mark whose colour indices are all unknown is dropped for the
        // same reason a `Niet toegewezen` one is.
        let cell = decode_cell(r#"{"marks":[{"lat":51.8,"lng":5.8,"k":0,"b":[42]}]}"#).unwrap();
        assert!(cell.marks.is_empty());
        assert!(decode_cell("{}").expect("empty is valid").marks.is_empty());
    }

    /// The local frame has its origin at the cell's north-west corner with +z
    /// running south. Get z the wrong way round and every buoy in the country is
    /// mirrored across its own cell — onto the bank.
    #[test]
    fn marks_project_into_the_cell_and_stray_ones_are_dropped() {
        let (cx, cy) = WAAL;
        let tile_m = 1500.0;
        let (north, west) = tile_to_lat_lng(cx, cy, MARK_ZOOM);
        let (mid_lat, mid_lng) = middle_of(cx, cy);
        let form = MarkForm {
            kind: MarkKind::Floating(MarkShape::Spar),
            body: ColourPattern::parse("Groen"),
            topmark: None,
            light: None,
        };
        let cell = MarkCell {
            marks: vec![
                (mid_lat, mid_lng, form.clone()),
                // Well north of the cell: the neighbour's mark, and the neighbour
                // draws it.
                (north + 0.01, west + 0.001, form),
            ],
        };
        let built = cell_marks(&cell, cx, cy, tile_m);
        assert_eq!(built.len(), 1, "the stray mark was kept");
        assert!(built[0].x > 0.0 && built[0].x < tile_m);
        assert!(built[0].z > 0.0 && built[0].z < tile_m);
        assert!(!cartopolis_geo::nav_marks::build_mark_meshes(&built, None).is_empty());
    }

    /// The URL the whole layer rests on, spelled out once: the path that exists
    /// (the obvious `…/rws/vaarwegmarkeringen/…` is a 404), CRS84 `bbox` order,
    /// and the limit the service caps at.
    #[test]
    fn the_items_url_is_the_one_that_answers() {
        let url = items_url(COLLECTIONS[0], WAAL.0, WAAL.1);
        assert!(url.starts_with(
            "https://api.pdok.nl/rws/vaarwegmarkeringen-nederland/ogc/v1\
             /collections/vaarweg_markeringen_drijvend_rd/items?f=json&limit=1000&bbox="
        ));
        let bbox = url.split("bbox=").nth(1).expect("a bbox");
        let parts: Vec<f64> = bbox.split(',').map(|v| v.parse().unwrap()).collect();
        assert_eq!(parts.len(), 4);
        assert!(parts[0] < parts[2], "west must precede east");
        assert!(parts[1] < parts[3], "south must precede north");
    }

    /// The two captured answers, read as the marks they describe.
    ///
    /// `W 12D` is a red can with a red light and **no** topmark (`tt_toptek` is
    /// `X`); the Waalhaven harbour light is a red-and-white banded post with a
    /// cylinder topmark and a red light, and both of its qualifying keys are the
    /// fixed collection's own.
    #[test]
    fn the_registers_own_answers_read_as_marks() {
        let floating = parse_items(REAL_FLOATING, true).expect("a real floating answer");
        assert_eq!(floating.len(), 1);
        let (lat, lng, form) = &floating[0];
        assert_eq!(form.kind, MarkKind::Floating(MarkShape::Can));
        assert_eq!(form.body.bands, vec![MarkColour::Red]);
        assert_eq!(form.light, Some(MarkColour::Red));
        assert!(form.topmark.is_none(), "`tt_toptek: X` is not a topmark");
        // On the Waal at Nijmegen, and the right way round: latitude ~51.85,
        // longitude ~5.86. Swap them and the buoy is in the Indian Ocean.
        assert!((51.0..52.0).contains(lat) && (5.0..6.0).contains(lng));

        let fixed = parse_items(REAL_FIXED, false).expect("a real fixed answer");
        assert_eq!(fixed.len(), 1);
        let form = &fixed[0].2;
        assert_eq!(form.kind, MarkKind::Fixed(FixedForm::LightPost));
        assert_eq!(form.body.bands, vec![MarkColour::Red, MarkColour::White]);
        assert!(
            form.body.repeating,
            "`… repeterend` was read as a plain pair"
        );
        assert_eq!(
            form.topmark.as_ref().map(|(t, _)| *t),
            Some(Topmark::Cylinder),
            "the fixed collection's topmark key was not read"
        );
        assert_eq!(form.light, Some(MarkColour::Red));

        // …and both survive the cache round trip and reach a mesh.
        let cell = MarkCell {
            marks: floating.into_iter().chain(fixed).collect(),
        };
        let back = decode_cell(&encode_cell(&cell)).expect("round trip");
        assert_eq!(back.marks.len(), 2);
        let built = cell_marks(&back, WAAL.0, WAAL.1, 1500.0);
        assert_eq!(built.len(), 2, "a real mark fell outside its own cell");
        assert!(!cartopolis_geo::nav_marks::build_mark_meshes(&built, None).is_empty());
    }

    #[test]
    fn the_cache_key_is_versioned_and_per_cell() {
        assert_eq!(cache_key(8458, 5422), "features/marks/v1/14/8458/5422.json");
    }
}
