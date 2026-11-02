//! The speed limit of the road ahead, for the sign over the speed disc.
//!
//! [`crate::systems::driver`] measures *where* you are on the route and draws the
//! chrome; this module answers what the law says about the road there. It cannot
//! be derived from a solved [`Route`] — a route is a polyline, a distance and a
//! list of turns, and it knows nothing about the 30 zone you are entering.
//!
//! It also used to find the services beside the route — fuel, charging, named car
//! parks, rest areas — and the user's saved places near it, for a rail down the
//! right edge of the navigation display. The rail went with the move to an
//! Organic Maps layout (2026-11-02), and the services query with it: an Overpass
//! answer nothing on screen reads is load on a rate-limited service for nothing.
//! Services as pins on the map is where they would come back.
//!
//! **One Overpass request per route, not per cell.** The streamed data layers
//! ([`crate::systems::data_layers`]) fetch a grid of cells around the *camera*;
//! this asks for a *corridor* instead, with `around` over the route's own
//! polyline, once, when the route changes. The answer is cached by the query, so
//! re-entering driver mode on the same route costs nothing.
//!
//! **Distances are in the driver's frame, not the planner's.** Everything here is
//! measured along [`RouteGeom`] — the pin lead-ins stripped, cumulative metres per
//! vertex — the same geometry `driver::Progress` is projected onto, so a limit
//! sampled at "9.4 km" is under the vehicle when the bar says 9.4 km are behind.

use crate::platform::sync::{Arc, Mutex};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;

use crate::platform::storage;
use crate::systems::driver::{nearest_on_polyline, DriverState, RouteGeom};
use crate::systems::navigation::RouteState;
use crate::systems::osm_buildings::overpass_query;

// ─── Tuning ──────────────────────────────────────────────────────────────────

/// Corridor half-width for the speed-limit query. Deliberately tight: this is
/// asking "which road am I on", and at 150 m every service road, cycleway and
/// parallel street would be a candidate answer.
const SPEED_CORRIDOR_M: u32 = 25;

/// A returned way's vertex has to land within this of the route to be accepted as
/// the road being driven. Slightly wider than [`SPEED_CORRIDOR_M`] would suggest,
/// because the routed line follows tile-simplified geometry and a dual
/// carriageway's two halves are drawn several metres apart.
const SPEED_MATCH_M: f64 = 20.0;

/// The speed limit is stored as one sample per this many metres of route, which
/// is finer than any real limit change and makes the lookup an array index. A
/// zone list would need a search and would gain nothing: a 100 km route is 4,000
/// samples, i.e. 8 KB.
pub const SPEED_SAMPLE_M: f64 = 25.0;

/// How finely a matched way's own geometry is walked when stamping its limit onto
/// the samples. Below [`SPEED_SAMPLE_M`], so a long straight way cannot skip a
/// sample and leave a hole in the middle of its own zone.
const SPEED_WALK_M: f64 = 12.0;

/// Vertices of the route polyline are dropped until they are this far apart before
/// going into the `around` filter. Overpass takes a linestring, so the segments
/// between kept vertices are still covered — this only trades query length for a
/// corridor that cuts a corner slightly, and it keeps a 40 km route's query a few
/// kilobytes rather than tens.
const CORRIDOR_STEP_M: f64 = 300.0;

/// Ceiling on how many vertices go into the `around` filter, whatever the route's
/// length. The step widens to meet it.
///
/// Since routing learned to stage a long haul over a coarser zoom
/// (`routing::plan_corridor`), a route can be 360 km rather than 22, and at a
/// fixed 300 m step that is 1,200 vertices — a query an order of magnitude past
/// what the constant above was sized for, over a corridor an order of magnitude
/// longer, which is how a single innocuous request becomes the one that gets the
/// client rate-limited. 400 points leaves a 120 km route untouched at its natural
/// step and widens only past that, where the route is motorway and nearly
/// straight, so the corner-cutting the step trades away costs almost nothing.
const MAX_CORRIDOR_POINTS: usize = 400;

// ─── Model ───────────────────────────────────────────────────────────────────

/// The result an [`IoTaskPool`] task hands back: one limit per sample.
type CorridorResult = Result<Vec<Option<u16>>, String>;

/// What is known about the road ahead of the vehicle.
#[derive(Resource, Default)]
pub struct RouteAhead {
    /// Speed limit in km/h per [`SPEED_SAMPLE_M`] of route; `None` where OSM has
    /// no `maxspeed` (which is most minor roads, and is why this is an option
    /// rather than a default).
    pub limits: Vec<Option<u16>>,
    /// `RouteState::generation` this belongs to. A change invalidates all of it.
    generation: Option<u32>,
    /// Where the corridor request leaves its answer.
    in_flight: Arc<Mutex<Option<CorridorResult>>>,
}

impl RouteAhead {
    /// The speed limit under a position `along_m` metres into the route.
    pub fn limit_at(&self, along_m: f64) -> Option<u16> {
        if along_m < 0.0 {
            return None;
        }
        let i = (along_m / SPEED_SAMPLE_M) as usize;
        self.limits.get(i).copied().flatten()
    }

    /// Drop everything; the route it described is gone.
    fn reset(&mut self) {
        self.limits.clear();
        self.generation = None;
        self.in_flight = Arc::default();
    }
}

// ─── The system ──────────────────────────────────────────────────────────────

/// Keep [`RouteAhead`] in step with the route being driven: ask Overpass for the
/// corridor once, take its answer when it lands.
///
/// Gated on driver mode being active. Planning a route deliberately costs nothing
/// on the network beyond the solve itself.
pub fn drive_route_ahead(
    mut ahead: ResMut<RouteAhead>,
    route: Res<RouteState>,
    driver: Res<DriverState>,
) {
    if !driver.active {
        if ahead.generation.is_some() {
            ahead.reset();
        }
        return;
    }
    // `displayed`, not `route`: a reroute in flight has no solved route for a
    // while, and the road past the missed turn is unchanged. The generation is
    // what identifies the polyline, and a reroute deliberately does not bump it
    // (`RouteState::reroute_from`), so the limits hold until a different route
    // lands.
    let Some(r) = route.displayed() else {
        if ahead.generation.is_some() {
            ahead.reset();
        }
        return;
    };

    // A new route (or an edited one) invalidates every metre measured against the
    // old polyline, so this starts over rather than trying to rebase.
    if ahead.generation != Some(route.generation) {
        ahead.reset();
        let Some(geom) = RouteGeom::build(r) else {
            return;
        };
        ahead.generation = Some(route.generation);
        let slot = ahead.in_flight.clone();
        let corridor = corridor_points(&geom);
        let points = geom.points.clone();
        let cum = geom.cum.clone();
        let total = geom.total_m;
        IoTaskPool::get()
            .spawn(async move {
                let built = fetch_corridor(&corridor, &points, &cum, total).await;
                if let Ok(mut g) = slot.lock() {
                    *g = Some(built);
                }
            })
            .detach();
        return;
    }

    let landed = ahead.in_flight.lock().ok().and_then(|mut g| g.take());
    match landed {
        Some(Ok(limits)) => {
            tracing::info!(
                limited = limits.iter().filter(|l| l.is_some()).count(),
                "route speed limits ready"
            );
            ahead.limits = limits;
        }
        Some(Err(e)) => tracing::warn!(error = %e, "route corridor fetch failed"),
        None => {}
    }
}

/// The polyline handed to Overpass' `around`, thinned to [`CORRIDOR_STEP_M`].
fn corridor_points(geom: &RouteGeom) -> Vec<[f64; 2]> {
    // Widen the step rather than truncate the route: a corridor that stops
    // two-thirds of the way along would leave the sign confidently blank for the
    // rest of the drive, which reads as "no limit" rather than "not asked".
    let step = CORRIDOR_STEP_M.max(geom.total_m / MAX_CORRIDOR_POINTS as f64);
    let mut out = Vec::new();
    let mut last_at = f64::NEG_INFINITY;
    for (i, p) in geom.points.iter().enumerate() {
        let at = geom.cum[i];
        if out.is_empty() || at - last_at >= step {
            out.push(*p);
            last_at = at;
        }
    }
    // The end matters as much as the start: without it the corridor stops short of
    // the destination by up to one step.
    if let Some(last) = geom.points.last() {
        if out.last() != Some(last) {
            out.push(*last);
        }
    }
    out
}

// ─── The corridor request ────────────────────────────────────────────────────

/// Ask Overpass for the speed limits along the corridor (`out geom`, so a way can
/// be matched against the route vertex by vertex).
async fn fetch_corridor(
    corridor: &[[f64; 2]],
    points: &[[f64; 2]],
    cum: &[f64],
    total_m: f64,
) -> CorridorResult {
    if corridor.len() < 2 {
        return Err("route too short for a corridor".into());
    }
    let query = corridor_query(corridor);
    let key = corridor_cache_key(&query);
    let text = match storage::get_string(storage::Namespace::Cache, &key).await {
        Some(cached) => cached,
        None => {
            let fetched = overpass_query(&query).await?;
            // Overpass reports a timeout or a rate-limit as HTTP 200 with a
            // `remark`, so validating before the write is what keeps a wobble from
            // being cached as "there is nothing along this route".
            if let Some(remark) = remark_of(&fetched) {
                return Err(format!("Overpass remark: {remark}"));
            }
            storage::put(storage::Namespace::Cache, &key, fetched.as_bytes()).await;
            fetched
        }
    };
    parse_corridor(&text, points, cum, total_m)
}

/// The Overpass QL for one corridor.
fn corridor_query(corridor: &[[f64; 2]]) -> String {
    let poly = corridor
        .iter()
        .map(|p| format!("{:.5},{:.5}", p[0], p[1]))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "[out:json][timeout:90];\
         way(around:{spd},{poly})[\"highway\"][\"maxspeed\"];\
         out geom;",
        spd = SPEED_CORRIDOR_M,
    )
}

/// Cache key for a corridor: the whole query string.
///
/// The polyline alone is the obvious key and it is subtly wrong — the query is
/// also *what was asked about it*, and the two change independently. Narrowing
/// which car parks count (as [`corridor_query`] does) would otherwise keep serving
/// the old broad answer from disk for every route already driven, with no way to
/// tell from the outside. Keying on the query makes a change to it a cache miss by
/// construction, which is the only version stamp that cannot be forgotten.
///
/// The coordinates inside it are already rounded to five decimals by
/// [`corridor_query`]'s formatting, so a route re-solved from the same endpoints
/// hashes the same despite the last bit of its floats differing.
fn corridor_cache_key(query: &str) -> String {
    let mut h = DefaultHasher::new();
    query.hash(&mut h);
    format!("route_ahead/{:016x}", h.finish())
}

/// Overpass' HTTP-200 error channel.
fn remark_of(text: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()?
        .get("remark")?
        .as_str()
        .map(|r| r.trim().to_string())
}

/// Turn one corridor response into a speed-limit sample array.
fn parse_corridor(text: &str, points: &[[f64; 2]], cum: &[f64], total_m: f64) -> CorridorResult {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if let Some(remark) = json.get("remark").and_then(|r| r.as_str()) {
        return Err(format!("Overpass remark: {}", remark.trim()));
    }
    let elements = json
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| "no elements in Overpass response".to_string())?;

    let samples = ((total_m / SPEED_SAMPLE_M).ceil() as usize).saturating_add(1);
    // Alongside each accepted limit, how far off the route the way that supplied
    // it was: where two roads are both inside the corridor — a service road beside
    // a trunk, the two halves of a dual carriageway — the nearer one is the one
    // being driven, and it must not depend on which came back first.
    let mut best: Vec<Option<(u16, f64)>> = vec![None; samples];
    for el in elements {
        let tags = el.get("tags");
        let tag = |k: &str| tags.and_then(|t| t.get(k)).and_then(|v| v.as_str());
        if let (Some(limit), Some(geometry)) = (
            tag("maxspeed").and_then(parse_maxspeed),
            el.get("geometry").and_then(|g| g.as_array()),
        ) {
            stamp_limit(&mut best, limit, geometry, points, cum);
        }
    }
    Ok(best.into_iter().map(|b| b.map(|(l, _)| l)).collect())
}

/// Walk a matched way's geometry and stamp its limit onto every route sample it
/// runs alongside.
///
/// Walked at [`SPEED_WALK_M`] rather than vertex by vertex: OSM ways are drawn
/// with as few vertices as the shape allows, so a straight kilometre of trunk road
/// is two points, and stamping only those would leave the forty samples between
/// them without a limit.
fn stamp_limit(
    best: &mut [Option<(u16, f64)>],
    limit: u16,
    geometry: &[serde_json::Value],
    points: &[[f64; 2]],
    cum: &[f64],
) {
    let coords: Vec<[f64; 2]> = geometry
        .iter()
        .filter_map(|v| {
            Some([
                v.get("lat").and_then(serde_json::Value::as_f64)?,
                v.get("lon").and_then(serde_json::Value::as_f64)?,
            ])
        })
        .collect();
    if coords.is_empty() {
        return;
    }
    let mut stamp = |at: [f64; 2]| {
        let (along, off) = nearest_on_polyline(points, cum, at);
        if off > SPEED_MATCH_M {
            return;
        }
        let i = (along / SPEED_SAMPLE_M) as usize;
        if let Some(slot) = best.get_mut(i) {
            if slot.is_none_or(|(_, prev)| off < prev) {
                *slot = Some((limit, off));
            }
        }
    };
    stamp(coords[0]);
    for w in coords.windows(2) {
        let span = metres_between(w[0], w[1]);
        let steps = (span / SPEED_WALK_M).ceil().max(1.0) as usize;
        for s in 1..=steps {
            let t = s as f64 / steps as f64;
            stamp([
                w[0][0] + (w[1][0] - w[0][0]) * t,
                w[0][1] + (w[1][1] - w[0][1]) * t,
            ]);
        }
    }
}

/// Metres between two `[lat, lng]`, on the local tangent plane (see
/// `driver::metres_between` — same approximation, same reason).
fn metres_between(a: [f64; 2], b: [f64; 2]) -> f64 {
    let mid = (a[0] + b[0]) * 0.5;
    let dx = (b[1] - a[1]) * 111_320.0 * mid.to_radians().cos();
    let dy = (b[0] - a[0]) * 110_540.0;
    (dx * dx + dy * dy).sqrt()
}

/// OSM `maxspeed` → km/h, or `None` when it is not a number this can show.
///
/// The tag is free-form. Most of the world writes a bare number in km/h; the
/// mph countries suffix it; and a large minority of values are *implicit* zone
/// codes (`NL:urban`, `DE:rural`) or advisory words (`none`, `walk`, `signals`).
/// The implicit ones are deliberately not mapped to their default limits: that
/// table is per country and changes, and a sign showing a confidently wrong
/// number is worse than no sign — the same reason the road class is not used as a
/// stand-in for a limit.
fn parse_maxspeed(raw: &str) -> Option<u16> {
    let raw = raw.trim().to_ascii_lowercase();
    let (number, factor) = match raw.strip_suffix("mph") {
        Some(n) => (n.trim(), 1.609_344),
        None => (
            raw.trim_end_matches("km/h").trim_end_matches("kph").trim(),
            1.0,
        ),
    };
    let value: f64 = number.parse().ok()?;
    if !(1.0..=200.0).contains(&value) {
        return None;
    }
    Some((value * factor).round() as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A straight 1 km route due north, in latitude steps (a degree of latitude is
    /// a constant 110.54 km, so the metres are exact).
    fn geom() -> (Vec<[f64; 2]>, Vec<f64>, f64) {
        let points: Vec<[f64; 2]> = (0..=10)
            .map(|i| [53.0 + f64::from(i) * 0.001, 6.0])
            .collect();
        let mut cum = vec![0.0];
        let mut total = 0.0;
        for w in points.windows(2) {
            total += metres_between(w[0], w[1]);
            cum.push(total);
        }
        (points, cum, total)
    }

    #[test]
    fn maxspeed_parses_the_forms_that_carry_a_number() {
        assert_eq!(parse_maxspeed("50"), Some(50));
        assert_eq!(parse_maxspeed(" 30 "), Some(30));
        assert_eq!(parse_maxspeed("120 km/h"), Some(120));
        assert_eq!(parse_maxspeed("70 mph"), Some(113));
        assert_eq!(parse_maxspeed("30mph"), Some(48));
    }

    #[test]
    fn maxspeed_refuses_to_guess_at_the_rest() {
        // An implicit zone code is a *country's* default, not a posted limit.
        assert_eq!(parse_maxspeed("NL:urban"), None);
        assert_eq!(parse_maxspeed("DE:rural"), None);
        assert_eq!(parse_maxspeed("none"), None);
        assert_eq!(parse_maxspeed("walk"), None);
        assert_eq!(parse_maxspeed("signals"), None);
        assert_eq!(parse_maxspeed(""), None);
        // Out of range: a mis-tagged 999 must not become a road sign.
        assert_eq!(parse_maxspeed("999"), None);
    }

    #[test]
    fn limits_index_by_metres_along() {
        let ahead = RouteAhead {
            limits: vec![Some(50), Some(50), None, Some(80)],
            ..default()
        };
        assert_eq!(ahead.limit_at(0.0), Some(50));
        assert_eq!(ahead.limit_at(SPEED_SAMPLE_M * 1.5), Some(50));
        assert_eq!(ahead.limit_at(SPEED_SAMPLE_M * 2.5), None);
        assert_eq!(ahead.limit_at(SPEED_SAMPLE_M * 3.5), Some(80));
        // Past the end of the route, and behind its start.
        assert_eq!(ahead.limit_at(SPEED_SAMPLE_M * 40.0), None);
        assert_eq!(ahead.limit_at(-5.0), None);
    }

    #[test]
    fn nearest_way_wins_a_sample() {
        let (points, cum, total) = geom();
        let samples = ((total / SPEED_SAMPLE_M).ceil() as usize) + 1;
        let mut best = vec![None; samples];
        let way = |lng: f64| {
            vec![
                serde_json::json!({ "lat": 53.0, "lon": lng }),
                serde_json::json!({ "lat": 53.002, "lon": lng }),
            ]
        };
        // A service road ~13 m east lands first, the driven road is on the line.
        stamp_limit(&mut best, 30, &way(6.0002), &points, &cum);
        stamp_limit(&mut best, 80, &way(6.0), &points, &cum);
        assert_eq!(best[2].map(|(l, _)| l), Some(80));
        // …and the order it arrived in must not matter.
        let mut best = vec![None; samples];
        stamp_limit(&mut best, 80, &way(6.0), &points, &cum);
        stamp_limit(&mut best, 30, &way(6.0002), &points, &cum);
        assert_eq!(best[2].map(|(l, _)| l), Some(80));
    }

    #[test]
    fn a_long_straight_way_stamps_every_sample_it_covers() {
        let (points, cum, total) = geom();
        let samples = ((total / SPEED_SAMPLE_M).ceil() as usize) + 1;
        let mut best = vec![None; samples];
        // Two vertices, 1 km apart — the case that vertex-only stamping would
        // leave forty holes in.
        let way = vec![
            serde_json::json!({ "lat": 53.0, "lon": 6.0 }),
            serde_json::json!({ "lat": 53.01, "lon": 6.0 }),
        ];
        stamp_limit(&mut best, 100, &way, &points, &cum);
        assert!(
            best.iter().take(samples - 1).all(Option::is_some),
            "every sample along the way should carry a limit"
        );
    }

    #[test]
    fn corridor_thins_the_polyline_but_keeps_the_end() {
        let (points, cum, total) = geom();
        let g = RouteGeom {
            points: points.clone(),
            cum,
            total_m: total,
            step_along: vec![],
        };
        let c = corridor_points(&g);
        assert!(
            c.len() < points.len(),
            "a 1 km route should thin below 11 points"
        );
        assert_eq!(c.first(), points.first());
        assert_eq!(c.last(), points.last(), "the destination end must survive");
    }

    /// A staged route can be 360 km, and at the natural 300 m step that is 1,200
    /// vertices in one `around` filter — the shape of request that gets a client
    /// rate-limited. The step must widen instead, and it must not do so on the
    /// routes it was tuned for.
    #[test]
    fn a_cross_country_corridor_stays_a_bounded_query() {
        // A straight north–south run, sampled every 100 m, at whatever length.
        let synthetic = |km: f64| {
            let n = (km * 10.0) as usize;
            let points: Vec<[f64; 2]> = (0..=n)
                .map(|i| [53.0 + i as f64 * 100.0 / 110_540.0, 6.0])
                .collect();
            let cum: Vec<f64> = (0..=n).map(|i| i as f64 * 100.0).collect();
            RouteGeom {
                points,
                total_m: cum[n],
                cum,
                step_along: vec![],
            }
        };

        for km in [40.0, 120.0, 360.0] {
            let c = corridor_points(&synthetic(km));
            assert!(
                c.len() <= MAX_CORRIDOR_POINTS + 1,
                "{km} km route asks for {} corridor points",
                c.len()
            );
        }
        // …and a route inside the old reach keeps its natural step, so this guard
        // cannot silently coarsen the corridors it was not written for.
        let short = corridor_points(&synthetic(20.0));
        assert!(
            (short.len() as f64 - 20_000.0 / CORRIDOR_STEP_M).abs() < 3.0,
            "a 20 km route thinned to {} points, not ~{:.0}",
            short.len(),
            20_000.0 / CORRIDOR_STEP_M
        );
    }

    #[test]
    fn corridor_cache_key_follows_the_route_and_the_question() {
        let a = corridor_query(&[[53.0, 6.0], [53.01, 6.0]]);
        let b = corridor_query(&[[53.0, 6.0], [53.02, 6.0]]);
        assert_eq!(corridor_cache_key(&a), corridor_cache_key(&a));
        assert_ne!(corridor_cache_key(&a), corridor_cache_key(&b));
        // A different question about the *same* corridor is a different key —
        // otherwise narrowing the query keeps serving the old broad answer.
        assert_ne!(
            corridor_cache_key(&a),
            corridor_cache_key(&a.replace("maxspeed", "name"))
        );
    }

    #[test]
    fn corridor_response_stamps_its_limits_along_the_route() {
        let (points, cum, total) = geom();
        let text = serde_json::json!({
            "elements": [
                { "type": "way", "id": 4,
                  "tags": { "highway": "residential", "maxspeed": "30" },
                  "geometry": [ { "lat": 53.0, "lon": 6.0 }, { "lat": 53.004, "lon": 6.0 } ] },
                // A way with no usable limit stamps nothing.
                { "type": "way", "id": 5,
                  "tags": { "highway": "primary", "maxspeed": "NL:urban" },
                  "geometry": [ { "lat": 53.006, "lon": 6.0 }, { "lat": 53.009, "lon": 6.0 } ] },
            ]
        })
        .to_string();
        let limits = parse_corridor(&text, &points, &cum, total).expect("parses");
        assert_eq!(limits[0], Some(30));
        assert_eq!(limits[30], None, "beyond the tagged way");
    }

    #[test]
    fn an_overpass_remark_is_an_error_not_an_empty_route() {
        let (points, cum, total) = geom();
        let text = r#"{"remark":"runtime error: Query timed out","elements":[]}"#;
        assert!(parse_corridor(text, &points, &cum, total).is_err());
    }
}
