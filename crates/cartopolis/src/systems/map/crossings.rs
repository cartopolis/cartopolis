//! The streaming half of the level crossings: ProRail's `overweg` collection,
//! the per-cell cache, and the projection into the frame the meshes are built in.
//!
//! The pure half — what stands on a crossing and how a cell's crossings become
//! meshes — is `cartopolis_geo::crossings`, and
//! `docs/notes/prorail-level-crossings.md` has the endpoint, the licence, the
//! census and the traps behind both.
//!
//! There is no streamer here and no resource, for the same reason
//! `systems::nav_marks` has none: a crossing ends up merged into its cell's
//! furniture mesh, so it has to be in hand on the worker that builds it rather
//! than arriving later on a schedule of its own. `map_geometry` awaits it beside
//! the navigation marks.
//!
//! # Why the client fetches this itself
//!
//! Exactly `nav_marks`' reasoning, and the same service host. The whole register
//! is 3,917 points behind a keyless `bbox` query answering
//! `Access-Control-Allow-Origin: *` with an hour of cache control on it; there is
//! nothing to reproject, nothing to triangulate and nothing to page nationally,
//! so an atlas extractor and a [`CoverageSlot`](crate::systems::coverage::CoverageSlot)
//! would both be machinery around a `GET`.
//!
//! # Three gates before a request is made
//!
//! Most of the planet is neither Dutch nor railed, and an empty answer still
//! costs a round trip and ~0.8 KB.
//!
//! 1. **The register's own extent** — see [`CROSSINGS_LON`] for why that is
//!    *measured* here rather than read off the collection document.
//! 2. **The cell's railway.** A crossing is where a road meets a track, and a
//!    cell with no rail in its tile cannot hold one. The tile body is in hand at
//!    this point but *not parsed* — the decode belongs on a worker (ADR-033) and
//!    the fetch decision has to be made before it — so this reads the undecoded
//!    body through [`vector_tiles::body_mentions_rail`], which cannot answer
//!    "railless" for a body holding a `kind = rail` street.
//! 3. **The Furniture toggle**, applied by the caller: crossings ride that
//!    group's mesh, its material and its altitude fade, so a run with furniture
//!    off asks for nothing at all.
//!
//! A fourth is about the service rather than the cell, and is `nav_marks`'
//! reasoning again: this fetch sits between the cell's tile body and its
//! tessellation worker, so a slow `api.pdok.nl` costs the ground, the roads and
//! the buildings rather than the barriers. The request carries
//! [`http::OPTIONAL_LAYER_TIMEOUT`], and a failure trips [`CROSSINGS_BREAKER`] —
//! a separate instance from the marks one, because the two registers are
//! published by different bodies and only share a host.
//!
//! # Three things the register says that it does not mean
//!
//! All three are recorded in the note as well, because each one is invisible when
//! it is got wrong:
//!
//! * **`levenscyclus_status` is not a built/not-built flag.** 1,304 of 3,917
//!   features read `Definitief ontwerp` — including the Peizerweg crossing in
//!   Groningen, which has been there for years — so filtering on it would delete
//!   a third of the register. Nothing here reads it. This is the trap
//!   `surveyed-street-objects.md` records for the BGT's retired trees, in the
//!   opposite direction: there, an obvious filter had to be *added*; here, an
//!   obvious filter must not be.
//! * **`aantal_ahob` is not a boom count.** The single-track Peizerweg crossing
//!   reports 15. How many booms an installation has comes from its form.
//! * **`azimut` is null on all 3,917 features**, as are `type_hek` and
//!   `aantal_andreas`. The orientation has to be derived from the road, which is
//!   what `cartopolis_geo::crossings::place_crossings` does.

use cartopolis_geo::crossings::{
    crossing_kind, CrossingKind, CrossingPoint, MAX_CROSSINGS_PER_CELL,
};

use crate::platform::cell_source::CellSource;
use crate::platform::http;
use crate::systems::surveyed::cell_local_unclamped;

/// Fixed cache/streaming zoom — must match `map_geometry::GEOMETRY_ZOOM`, since a
/// crossing is meshed into that cell.
const CROSSING_ZOOM: u32 = 14;

/// PDOK's OGC API host, the same one `systems::nav_marks` and `systems::aerial`
/// reach: `Access-Control-Allow-Origin: *`, so the web build needs no relay.
const CROSSINGS_ORIGIN: &str = "https://api.pdok.nl";

/// The service's entry point. Note the path: the register is published as
/// *Spoorwegen*, not as any of the collection names inside it.
const CROSSINGS_PATH: &str = "/prorail/spoorwegen/ogc/v1";

/// One of the service's seven collections, and the only one that changes the
/// picture — `cartopolis_geo::crossings`' module note has the other six and why
/// each was rejected.
const COLLECTION: &str = "overweg";

/// The service caps `limit` at 1000 and answers `200` with 1000 features for
/// anything larger. The whole country holds 3,917 crossings, so a z14 cell that
/// came back full would mean something is very wrong.
const PAGE_LIMIT: usize = 1000;

/// The register's extent, **measured** over all 3,917 features on 2026-08-30 and
/// padded to the nearest tenth of a degree.
///
/// Deliberately not the collection document's own `extent.spatial.bbox`, which is
/// `-1.66, 48.04 → 12.43, 56.11` — Rennes to Copenhagen. That is the RD grid
/// (EPSG:28992, the collection's storage CRS) reprojected to CRS84, not a
/// statement about where ProRail has railways, and using it verbatim would let
/// every cell in Belgium, northern France, Germany and Denmark through the first
/// gate. `systems::aerial` already records the general rule — coverage is an
/// envelope, never a status code — and this is its sharper form: the envelope a
/// service publishes may be its *projection's*, not its data's.
const CROSSINGS_LON: (f64, f64) = (3.5, 7.4);
const CROSSINGS_LAT: (f64, f64) = (50.7, 53.5);

// ─── The cached form ─────────────────────────────────────────────────────────

/// The forms, in cache order. **Append, never renumber** — and renumbering means
/// bumping the `v1` in [`cache_key`], because a cached cell written by an older
/// table would otherwise be read back as the wrong installations.
const KINDS: [CrossingKind; 4] = [
    CrossingKind::HalfBarrier,
    CrossingKind::MiniBarrier,
    CrossingKind::FullBarrier,
    CrossingKind::Lights,
];

/// One crossing as it is cached: the position, the form, and the track count.
///
/// **Not the GeoJSON.** A feature carries fifty properties and runs to ~2 KB —
/// maintenance region, operating area, kilometre lint, publication date — and
/// `properties=` subsetting is a 400 on this service, the same shape `nav_marks`
/// found on the RWS one. Everything that reaches the meshes is in these four
/// fields.
#[derive(serde::Serialize, serde::Deserialize)]
struct WireCrossing {
    lat: f64,
    lng: f64,
    /// Index into [`KINDS`].
    k: usize,
    /// `aantal_sporen`.
    #[serde(default)]
    t: u8,
}

/// One cell's crossings, still in lat/lng as the register holds them.
#[derive(Clone, Debug, Default)]
pub struct CrossingCell {
    pub crossings: Vec<(f64, f64, CrossingKind, u8)>,
}

fn encode_cell(cell: &CrossingCell) -> String {
    let crossings: Vec<WireCrossing> = cell
        .crossings
        .iter()
        .filter_map(|(lat, lng, kind, tracks)| {
            Some(WireCrossing {
                lat: *lat,
                lng: *lng,
                k: KINDS.iter().position(|k| k == kind)?,
                t: *tracks,
            })
        })
        .collect();
    serde_json::json!({ "crossings": crossings }).to_string()
}

/// Read a cell back out of the cache.
///
/// An index this build has no form for drops the crossing rather than clamping it
/// onto the last known one — the same rule `nav_marks`' `KINDS` and `surveyed`'s
/// `PROP_KINDS` follow, and the same reason: a barrier drawn where the register
/// says lights says the wrong thing.
pub fn decode_cell(text: &str) -> Result<CrossingCell, String> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        crossings: Vec<WireCrossing>,
    }
    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(CrossingCell {
        crossings: wire
            .crossings
            .into_iter()
            .filter_map(|c| Some((c.lat, c.lng, *KINDS.get(c.k)?, c.t)))
            .collect(),
    })
}

// ─── The register ────────────────────────────────────────────────────────────

/// Does the register cover this cell at all?
///
/// Tested on the cell's own corners rather than its centre, so a border cell half
/// inside the envelope is still asked about.
#[must_use]
pub fn covers(cx: i64, cy: i64) -> bool {
    let (north, west) = cartopolis_core::world::tile_to_lat_lng(cx, cy, CROSSING_ZOOM);
    let (south, east) = cartopolis_core::world::tile_to_lat_lng(cx + 1, cy + 1, CROSSING_ZOOM);
    west <= CROSSINGS_LON.1
        && east >= CROSSINGS_LON.0
        && south <= CROSSINGS_LAT.1
        && north >= CROSSINGS_LAT.0
}

/// The cell's own two gates: could this cell hold a registered crossing?
///
/// True of the cell forever, so this is also what decides whether the cache is
/// consulted at all — a foreign or railless cell must cost neither a request nor
/// a storage lookup.
fn cell_may_hold_crossings(cx: i64, cy: i64, tile_has_rail: bool) -> bool {
    covers(cx, cy) && tile_has_rail
}

/// ProRail's crossings service, held off for a minute after it fails. Its own
/// instance: RWS's marks service sits behind the same host and fails separately.
pub(crate) static CROSSINGS_BREAKER: http::Breaker = http::Breaker::new();

fn cache_key(cx: i64, cy: i64) -> String {
    format!("features/crossings/v1/{CROSSING_ZOOM}/{cx}/{cy}.json")
}

fn items_url(cx: i64, cy: i64) -> String {
    let (north, west) = cartopolis_core::world::tile_to_lat_lng(cx, cy, CROSSING_ZOOM);
    let (south, east) = cartopolis_core::world::tile_to_lat_lng(cx + 1, cy + 1, CROSSING_ZOOM);
    format!(
        "{CROSSINGS_ORIGIN}{CROSSINGS_PATH}/collections/{COLLECTION}/items\
         ?f=json&limit={PAGE_LIMIT}&bbox={west},{south},{east},{north}"
    )
}

/// Pull one property as a string. The register writes `aantal_sporen` as a JSON
/// number and `overweg_type` as a string, so a `Value::String`-only read would
/// silently drop the track count — the same trap the `pois` house numbers had.
fn prop<'a>(feature: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    feature.get("properties")?.get(key)
}

/// The first coordinate of a feature's geometry.
///
/// Every `overweg` feature sampled is a plain `Point`, unlike the RWS marks,
/// which are `MultiPoint`s of one. `MultiPoint` is accepted anyway, because the
/// failure when a geometry type goes unread is an empty layer that looks exactly
/// like a region with no crossings in it.
fn position(feature: &serde_json::Value) -> Option<(f64, f64)> {
    let geometry = feature.get("geometry")?;
    let coords = geometry.get("coordinates")?;
    let pair = match geometry.get("type")?.as_str()? {
        "Point" => coords,
        "MultiPoint" => coords.get(0)?,
        _ => return None,
    };
    // CRS84: longitude first. The storage CRS is EPSG:28992 and the default
    // output CRS is CRS84, which is what is asked for.
    let lng = pair.get(0)?.as_f64()?;
    let lat = pair.get(1)?.as_f64()?;
    Some((lat, lng))
}

/// Turn one GeoJSON feature into a crossing, or drop it.
///
/// **Nothing here reads `levenscyclus_status`**, and the module note says why:
/// a third of the register reads `Definitief ontwerp` while standing in the
/// street.
fn read_feature(feature: &serde_json::Value) -> Option<(f64, f64, CrossingKind, u8)> {
    let kind = crossing_kind(prop(feature, "overweg_type")?.as_str()?)?;
    // A missing or unreadable count is one track, not none: `clearance_m` treats
    // zero the same way, and 68 features register zero over a road that plainly
    // crosses something.
    let tracks = prop(feature, "aantal_sporen")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1)
        .min(u64::from(u8::MAX)) as u8;
    let (lat, lng) = position(feature)?;
    Some((lat, lng, kind, tracks))
}

/// Parse the collection's `items` response.
pub fn parse_items(text: &str) -> Result<Vec<(f64, f64, CrossingKind, u8)>, String> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let features = json
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| "no features in the crossings response".to_string())?;
    // A full page means the cursor has more. At z14 it never should — the whole
    // country holds 3,917 — so this is a loud line rather than a silent second
    // page.
    if features.len() >= PAGE_LIMIT {
        tracing::warn!(
            limit = PAGE_LIMIT,
            "level crossings came back a full page; this cell is marked short"
        );
    }
    Ok(features.iter().filter_map(read_feature).collect())
}

/// Fetch and parse one cell's crossings, through the storage cache.
///
/// `tile_has_rail` is the second gate: see the module note for why it reads an
/// undecoded body rather than a parse. An ungated cell, an unreachable service
/// and an empty answer all produce the same empty cell, which is the bare
/// railway this client already drew.
pub(crate) async fn load_crossings_cell(cx: i64, cy: i64, tile_has_rail: bool) -> CrossingCell {
    // The tile's own railway is the gate that cannot live in the descriptor: it
    // is a property of *this* request (the caller read the undecoded body), not
    // of the cell. The register's extent is the descriptor's.
    if !cell_may_hold_crossings(cx, cy, tile_has_rail) {
        return CrossingCell::default();
    }
    CROSSINGS
        .load_with(cx, cy, fetch_and_normalise(cx, cy))
        .await
}

/// The register's extent, the cache, the breaker and the store — the seven steps
/// every additive per-cell layer shares. Only the fetch below is this module's.
const CROSSINGS: CellSource<CrossingCell> = CellSource {
    label: "level crossings",
    gate: Some(covers),
    key: cache_key,
    url: Some(items_url),
    breaker: Some(&CROSSINGS_BREAKER),
    timeout: Some(http::OPTIONAL_LAYER_TIMEOUT),
    parse: |text| decode_cell(text).ok(),
};

/// The collection, normalised into the compact stored form — or `None`, which is
/// what trips the breaker.
///
/// Nothing is written to the cache on a failed or unreadable answer: a poisoned
/// key is never re-fetched, so an empty cell cached under one is worse than a
/// cell that is asked for again. What *is* remembered is that the service failed,
/// and that is the breaker rather than the cache.
async fn fetch_and_normalise(cx: i64, cy: i64) -> Option<String> {
    let request = http::Request::get(items_url(cx, cy)).timeout(http::OPTIONAL_LAYER_TIMEOUT);
    let text = http::fetch_string(request).await.ok()?;
    let mut crossings = match parse_items(&text) {
        Ok(read) => read,
        Err(e) => {
            tracing::warn!(error = %e, "level crossings unreadable");
            return None;
        }
    };
    if crossings.len() > MAX_CROSSINGS_PER_CELL {
        tracing::warn!(
            cap = MAX_CROSSINGS_PER_CELL,
            found = crossings.len(),
            "level crossings hit the per-cell cap; this cell is marked short"
        );
        crossings.truncate(MAX_CROSSINGS_PER_CELL);
    }
    Some(encode_cell(&CrossingCell { crossings }))
}

/// This cell's crossings in the local metre frame, ready to be oriented and
/// meshed.
///
/// A crossing outside the cell is dropped rather than drawn: the bbox query is
/// the cell's own, so this only catches the boundary case, and a crossing is a
/// point that belongs to exactly one cell — drawing it from two would stand two
/// installations on one road.
#[must_use]
pub fn cell_crossings(cell: &CrossingCell, cx: i64, cy: i64, tile_m: f32) -> Vec<CrossingPoint> {
    cell.crossings
        .iter()
        .filter_map(|(lat, lng, kind, tracks)| {
            let (x, z) = cell_local_unclamped(*lat, *lng, cx, cy, CROSSING_ZOOM, tile_m);
            (x >= 0.0 && z >= 0.0 && x < tile_m && z < tile_m).then_some(CrossingPoint {
                x,
                z,
                kind: *kind,
                tracks: *tracks,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::world::tile_to_lat_lng;

    /// The predicate `load_crossings_cell` applies, spelled out: the cell's own
    /// two gates and the service's outage gate. `CROSSINGS.would_fetch` is the
    /// production call, so the only thing restated here is the composition with
    /// the tile's railway — the one gate that belongs to the request rather than
    /// to the cell.
    fn should_fetch(
        cx: i64,
        cy: i64,
        tile_has_rail: bool,
        breaker: &http::Breaker,
        now: web_time::Instant,
    ) -> bool {
        cell_may_hold_crossings(cx, cy, tile_has_rail)
            && CROSSINGS.would_fetch(cx, cy, breaker.is_open_at(now))
    }

    /// Two real answers from the service, captured verbatim on 2026-08-30 (the
    /// `links` array trimmed, the Utrecht one cut to two features).
    ///
    /// The hand-written fixtures below are my idea of the shape; these two are
    /// the register's, and they pin the things that are invisible when they are
    /// wrong — the geometry is a bare `Point`, `aantal_sporen` is a JSON *number*
    /// and not a string, and the Peizerweg crossing carries all three of the
    /// traps at once.
    ///
    /// Ordinary text files, deliberately: `.gitattributes` does not track
    /// `.json`, so these are not LFS objects and cannot arrive as pointer files
    /// (CLAUDE.md's standing trap).
    const REAL_GRONINGEN: &str =
        include_str!("../../../tests/fixtures/level_crossings_groningen.json");
    const REAL_UTRECHT: &str = include_str!("../../../tests/fixtures/level_crossings_utrecht.json");

    /// The z14 cell holding the Peizerweg AHOB crossing, which is the scene gate.
    const GRONINGEN_RAIL: (i64, i64) = (8490, 5321);
    /// Groningen's Grote Markt: inside the envelope, no railway in the cell, and
    /// no crossings in the register.
    const GRONINGEN_CENTRE: (i64, i64) = (8490, 5320);

    fn middle_of(cx: i64, cy: i64) -> (f64, f64) {
        let (north, west) = tile_to_lat_lng(cx, cy, CROSSING_ZOOM);
        let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, CROSSING_ZOOM);
        ((north + south) / 2.0, (west + east) / 2.0)
    }

    /// The real Groningen answer, which is the Peizerweg AHOB and nothing else.
    #[test]
    fn the_real_answer_reads_as_one_half_barrier() {
        let read = parse_items(REAL_GRONINGEN).expect("the captured answer parses");
        assert_eq!(read.len(), 1, "the Peizerweg crossing");
        let (lat, lng, kind, tracks) = read[0];
        assert_eq!(kind, CrossingKind::HalfBarrier);
        assert_eq!(tracks, 1, "aantal_sporen is a JSON number, not a string");
        assert!((lat - 53.2102).abs() < 0.001, "lat {lat}");
        assert!((lng - 6.5493).abs() < 0.001, "lng {lng}");
    }

    /// The three fields the register fills in with something that looks like an
    /// answer and is not one. Each is asserted against the *captured* feature,
    /// so this is a claim about the data rather than about my summary of it.
    #[test]
    fn the_three_traps_are_in_the_captured_feature() {
        let json: serde_json::Value =
            serde_json::from_str(REAL_GRONINGEN).expect("the fixture parses");
        let props = &json["features"][0]["properties"];

        // 1. The lifecycle status is not a built/not-built flag: this crossing is
        //    on the Peizerweg and reads `Definitief ontwerp`. Nothing filters on
        //    it, and `read_feature` returning the crossing is that fact.
        assert_eq!(props["levenscyclus_status"], "Definitief ontwerp");
        assert_eq!(
            parse_items(REAL_GRONINGEN).unwrap().len(),
            1,
            "a lifecycle filter crept in and deleted a third of the register"
        );

        // 2. `aantal_ahob` is an asset tally, not a boom count: fifteen of them
        //    on a single-track crossing.
        assert_eq!(props["aantal_ahob"], 15);
        assert_eq!(props["aantal_sporen"], 1);

        // 3. There is no orientation in the register at all, which is why the
        //    yaw is derived from the road.
        for null in ["azimut", "type_hek", "aantal_andreas"] {
            assert!(props[null].is_null(), "{null} is no longer null");
        }
    }

    /// Utrecht Centraal's cell, cut to the two answers that matter: an `AOB` is
    /// drawn as full barriers, and a `Geen` — a crossing the register says has no
    /// installation on it — is drawn as nothing.
    #[test]
    fn geen_is_dropped_from_a_real_answer() {
        let json: serde_json::Value =
            serde_json::from_str(REAL_UTRECHT).expect("the fixture parses");
        assert_eq!(
            json["features"].as_array().map(Vec::len),
            Some(2),
            "the fixture should carry both cases"
        );
        let read = parse_items(REAL_UTRECHT).expect("the captured answer parses");
        assert_eq!(read.len(), 1, "the `Geen` feature should have been dropped");
        assert_eq!(read[0].2, CrossingKind::FullBarrier);
    }

    /// The two gates that keep this layer from asking the register about the rest
    /// of the planet, and about the railless nine-tenths of the Netherlands.
    ///
    /// Asserted on the predicate rather than on the fetch, because "no request
    /// was issued" is not observable from the other side of an `async fn` that
    /// returns the same empty cell either way.
    #[test]
    fn no_request_is_made_outside_the_envelope_or_off_the_rails() {
        let (breaker, now) = (http::Breaker::new(), web_time::Instant::now());
        assert!(should_fetch(
            GRONINGEN_RAIL.0,
            GRONINGEN_RAIL.1,
            true,
            &breaker,
            now
        ));
        assert!(
            !should_fetch(GRONINGEN_RAIL.0, GRONINGEN_RAIL.1, false, &breaker, now),
            "a cell with no railway in its tile must not be asked about"
        );
        assert!(!cell_may_hold_crossings(
            GRONINGEN_RAIL.0,
            GRONINGEN_RAIL.1,
            false
        ));
        // Rome, Reykjavík and a point in the Atlantic — all outside the
        // register's envelope, so a railway in the tile buys them nothing.
        for (lat, lng) in [(41.9, 12.5), (64.1, -21.9), (30.0, -40.0)] {
            let (cx, cy) = cartopolis_core::world::lat_lng_to_tile(lat, lng, CROSSING_ZOOM);
            assert!(
                !should_fetch(cx, cy, true, &breaker, now),
                "{lat},{lng} is not in the Dutch rail register"
            );
        }
    }

    /// The gate the outage adds: a cell that would otherwise ask is refused for
    /// a minute after the service failed, and asks again after it.
    #[test]
    fn a_failed_service_is_not_asked_again_until_the_cooldown_passes() {
        let breaker = http::Breaker::new();
        let now = web_time::Instant::now();
        let (cx, cy) = GRONINGEN_RAIL;
        assert!(should_fetch(cx, cy, true, &breaker, now));

        breaker.trip_at(now);
        assert!(
            !should_fetch(cx, cy, true, &breaker, now),
            "a cell was asked about while the service was known to be down"
        );
        assert!(should_fetch(
            cx,
            cy,
            true,
            &breaker,
            now + http::BREAKER_COOLDOWN
        ));
    }

    /// One breaker per service, not one per host. RWS publishes the navigation
    /// marks and ProRail the crossings; they answer from the same `api.pdok.nl`
    /// and can fail apart, so an outage on one must not blank the other.
    ///
    /// This is the one test that touches the two process-wide statics, which is
    /// safe because nothing else in the suite reads them — every other assertion
    /// injects a breaker of its own.
    #[test]
    fn one_registers_outage_does_not_gate_the_other() {
        let now = web_time::Instant::now();
        crate::systems::nav_marks::MARKS_BREAKER.trip_at(now);
        assert!(crate::systems::nav_marks::MARKS_BREAKER.is_open_at(now));
        assert!(
            !CROSSINGS_BREAKER.is_open_at(now),
            "the crossings service was held off by the marks service failing"
        );
    }

    /// The envelope is the *register's*, not the collection document's — which
    /// reaches Rennes and Copenhagen because it describes the RD grid rather than
    /// the data in it.
    ///
    /// What it is **not** is a border: an envelope is a rectangle, so Brussels
    /// and Cologne are inside the one the Dutch corners describe and each costs
    /// one empty request. That is the trade this gate makes, and the right way
    /// round — `aerial` records the same rule, and a gate tight enough to
    /// exclude Brussels would clip Zeeland.
    #[test]
    fn the_published_extent_is_not_the_envelope_used() {
        for (name, lat, lng) in [
            ("Berlin", 52.52, 13.40),
            ("Copenhagen", 55.68, 12.57),
            ("Rennes", 48.11, -1.68),
            ("Paris", 48.86, 2.35),
        ] {
            let (cx, cy) = cartopolis_core::world::lat_lng_to_tile(lat, lng, CROSSING_ZOOM);
            assert!(
                !covers(cx, cy),
                "{name} is inside the collection's published bbox and must still be refused"
            );
        }
        // …and the country itself is in, corner to corner.
        for (name, lat, lng) in [
            ("Maastricht", 50.85, 5.69),
            ("Delfzijl", 53.33, 6.92),
            ("Vlissingen", 51.44, 3.57),
            ("Den Helder", 52.96, 4.76),
        ] {
            let (cx, cy) = cartopolis_core::world::lat_lng_to_tile(lat, lng, CROSSING_ZOOM);
            assert!(covers(cx, cy), "{name} is in the register and was refused");
        }
    }

    /// The cache round-trips every form, and a form index this build has no entry
    /// for drops the crossing rather than clamping it onto the last one.
    #[test]
    fn the_cache_round_trips_and_drops_unknown_forms() {
        let cell = CrossingCell {
            crossings: KINDS
                .iter()
                .enumerate()
                .map(|(i, k)| (53.2 + i as f64 * 0.001, 6.5, *k, i as u8 + 1))
                .collect(),
        };
        let back = decode_cell(&encode_cell(&cell)).expect("the cache round-trips");
        assert_eq!(back.crossings, cell.crossings);

        let bogus = decode_cell(r#"{"crossings":[{"lat":53.2,"lng":6.5,"k":99,"t":1}]}"#)
            .expect("an unknown form still parses");
        assert!(bogus.crossings.is_empty());
    }

    /// A crossing in the cell lands inside it; one in the neighbour is dropped,
    /// so the same installation is never built twice from two cells.
    #[test]
    fn a_crossing_belongs_to_exactly_one_cell() {
        let tile_m = 611.0;
        let (lat, lng) = middle_of(GRONINGEN_RAIL.0, GRONINGEN_RAIL.1);
        let cell = CrossingCell {
            crossings: vec![(lat, lng, CrossingKind::HalfBarrier, 2)],
        };
        assert_eq!(
            cell_crossings(&cell, GRONINGEN_RAIL.0, GRONINGEN_RAIL.1, tile_m).len(),
            1
        );
        assert!(
            cell_crossings(&cell, GRONINGEN_CENTRE.0, GRONINGEN_CENTRE.1, tile_m).is_empty(),
            "a crossing was claimed by a cell it is not in"
        );
    }

    /// The request the gates let through, so a path typo is caught here rather
    /// than as a layer that silently draws nothing.
    #[test]
    fn the_url_names_the_collection_and_the_cell() {
        let url = items_url(GRONINGEN_RAIL.0, GRONINGEN_RAIL.1);
        assert!(url.starts_with("https://api.pdok.nl/prorail/spoorwegen/ogc/v1/"));
        assert!(url.contains("/collections/overweg/items"));
        assert!(url.contains("limit=1000"));
        let (north, west) = tile_to_lat_lng(GRONINGEN_RAIL.0, GRONINGEN_RAIL.1, CROSSING_ZOOM);
        assert!(
            url.contains(&format!("bbox={west},")),
            "the bbox must lead with longitude (CRS84): {url}"
        );
        assert!(url.contains(&format!(",{north}")));
    }
}
