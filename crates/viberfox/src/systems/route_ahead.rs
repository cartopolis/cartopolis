//! What lies ahead on the route being driven: the services beside it, the user's
//! own saved places near it, and the speed limit of the road under the vehicle.
//!
//! [`crate::systems::driver`] measures *where* you are on the route and draws the
//! chrome; this module answers *what is coming*, which is the other half of a
//! navigation display. Both reference layouts this was specified against carry it:
//! a rail down the right edge running from the vehicle to the destination with the
//! things you will pass marked along it in proportion, and a speed-limit sign
//! beside the speedometer. Neither can be derived from a solved [`Route`] — a
//! route is a polyline, a distance and a list of turns, and it knows nothing about
//! the petrol station at kilometre nine or the 30 zone you are entering.
//!
//! **One Overpass request per route, not per cell.** The streamed data layers
//! ([`crate::systems::data_layers`]) fetch a grid of cells around the *camera*,
//! which is the wrong shape here twice over: driver mode's minimal mode turns
//! every layer off, and a rail that only knew about the cells near you would leave
//! the far two thirds of the route blank — exactly the part the widget exists to
//! show. So this asks Overpass for a *corridor* instead, with `around` over the
//! route's own polyline, once, when the route changes. The answer is cached by the
//! shape of that polyline, so re-entering driver mode on the same route costs
//! nothing.
//!
//! **The rail fills in two stages and is useful before the network answers.**
//! Saved places are local, so they are projected onto the route the frame the
//! route changes; the corridor services and the speed limits merge in when the
//! request lands. A rail that stayed empty until Overpass replied would be blank
//! for the first seconds of every drive, which is when it is looked at most.
//!
//! **Distances are in the driver's frame, not the planner's.** Everything here is
//! measured along [`RouteGeom`] — the pin lead-ins stripped, cumulative metres per
//! vertex — the same geometry `driver::Progress` is projected onto. A rail item at
//! "9.4 km" and a readout saying "9.4 km to go" therefore mean the same thing;
//! measuring one against `Route::distance_m` and the other against the polyline
//! would put them a few dozen metres apart and there would be no way to tell which
//! was lying.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;

use crate::platform::storage;
use crate::systems::driver::{nearest_on_polyline, DriverState, RouteGeom};
use crate::systems::navigation::RouteState;
use crate::systems::osm_buildings::overpass_query;
use crate::systems::user_store::UserStore;

// ─── Tuning ──────────────────────────────────────────────────────────────────

/// Corridor half-width for the services query, in metres. Wide enough to catch a
/// filling station set back behind its forecourt, narrow enough that a parallel
/// high street does not empty itself onto the rail.
const POI_CORRIDOR_M: u32 = 150;

/// …and for the rest areas, which are laid out along motorways at a greater
/// stand-off than a town-centre amenity.
const REST_CORRIDOR_M: u32 = 300;

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

/// A POI has to be within this of the route to be worth a rail marker. Wider than
/// the query corridor is pointless; narrower discards things the query paid for.
const POI_NEAR_M: f64 = POI_CORRIDOR_M as f64 + 20.0;

/// A saved place gets a rail marker within this of the route. Generous compared
/// with the services: a place the *user* saved is worth mentioning even when it is
/// a couple of streets off, and there are only ever tens of them.
const SAVED_NEAR_M: f64 = 400.0;

/// Vertices of the route polyline are dropped until they are this far apart before
/// going into the `around` filter. Overpass takes a linestring, so the segments
/// between kept vertices are still covered — this only trades query length for a
/// corridor that cuts a corner slightly, and it keeps a 40 km route's query a few
/// kilobytes rather than tens.
const CORRIDOR_STEP_M: f64 = 300.0;

/// Ceiling on rail markers kept from one corridor. The rail is 240 px tall and
/// can show perhaps a dozen; the rest are counted, not drawn.
const MAX_ITEMS: usize = 24;

/// …and how many of them may be parking.
///
/// Measured, not guessed: a 4.8 km route across Groningen came back with 45
/// amenities, all but two of them parking, and the rail rendered as a column of
/// identical `P` badges with a "+38" — the one charging point on the route was
/// pushed off it. Parking is the most numerous driving amenity in any city centre
/// and the least urgent (you look for it at the end of a drive, not during one),
/// so it gets a fixed minority of the rail and fuel/charging/rest keep their
/// places. The query is narrowed too — see [`corridor_query`] — but a cap is what
/// makes the guarantee hold in a city this rule did not anticipate.
const MAX_PARKING: usize = 6;

// ─── Model ───────────────────────────────────────────────────────────────────

/// What a rail marker is. Deliberately a small closed set rather than the
/// data-layer categories: this is a *driving* readout, so it carries the things
/// a drive is interrupted for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AheadKind {
    Fuel,
    Charging,
    Parking,
    RestArea,
    /// One of the user's own places from the Places tree.
    Saved,
    /// The end of the route. Always present, always last.
    Destination,
}

impl AheadKind {
    /// Preferred glyphs, most wanted first, in the same probe-and-fall-back form
    /// `routing::Maneuver::icon_candidates` uses — egui ships a thin emoji subset
    /// and a missing glyph paints an empty box.
    pub fn icon_candidates(self) -> &'static [&'static str] {
        match self {
            Self::Fuel => &["⛽", "F"],
            Self::Charging => &["⚡", "E"],
            Self::Parking => &["🅿", "P"],
            Self::RestArea => &["☕", "R"],
            Self::Saved => &["★", "*"],
            Self::Destination => &["🏁", "⚑", "X"],
        }
    }

    /// What it is called when there is no name to show.
    pub fn label(self) -> &'static str {
        match self {
            Self::Fuel => "Fuel",
            Self::Charging => "Charging",
            Self::Parking => "Parking",
            Self::RestArea => "Rest area",
            Self::Saved => "Saved place",
            Self::Destination => "Destination",
        }
    }

    /// Sort weight when two markers land on the same pixel and one has to win.
    /// The destination outranks everything; a place the user saved outranks a
    /// generic amenity, because they put it there.
    fn priority(self) -> u8 {
        match self {
            Self::Destination => 3,
            Self::Saved => 2,
            Self::Fuel | Self::Charging => 1,
            Self::Parking | Self::RestArea => 0,
        }
    }
}

/// One thing you will pass, placed on the route by projection.
#[derive(Clone, Debug)]
pub struct AheadItem {
    /// Metres from the start of the driven polyline — the same frame as
    /// `driver::Progress::along_m`.
    pub along_m: f64,
    /// Metres from the route to the thing itself.
    pub off_m: f64,
    pub kind: AheadKind,
    /// Its name, when OSM or the user gave it one.
    pub name: Option<String>,
}

impl AheadItem {
    /// What to show for it: its name, else what it is.
    pub fn title(&self) -> &str {
        self.name.as_deref().unwrap_or_else(|| self.kind.label())
    }
}

/// Where the corridor request has got to, so the rail can say so instead of
/// looking empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum AheadStatus {
    /// No route, or nothing asked for yet.
    #[default]
    Idle,
    Fetching,
    Ready,
    /// The corridor request failed. The rail still works — saved places and the
    /// destination are local — so this is a caption, not an error state.
    Failed(String),
}

/// The result an [`IoTaskPool`] task hands back.
type CorridorResult = Result<(Vec<AheadItem>, Vec<Option<u16>>), String>;

/// Everything known about the road ahead of the vehicle.
#[derive(Resource, Default)]
pub struct RouteAhead {
    /// Markers, sorted by `along_m`, in the driven polyline's frame.
    pub items: Vec<AheadItem>,
    /// Speed limit in km/h per [`SPEED_SAMPLE_M`] of route; `None` where OSM has
    /// no `maxspeed` (which is most minor roads, and is why this is an option
    /// rather than a default).
    pub limits: Vec<Option<u16>>,
    pub status: AheadStatus,
    /// Length of the polyline the above were measured against, so the rail never
    /// scales items by a route they do not belong to.
    pub total_m: f64,
    /// `RouteState::generation` this belongs to. A change invalidates all of it.
    generation: Option<u32>,
    /// Set while the corridor request is out, so a slow reply is not re-requested
    /// every frame.
    in_flight: Arc<Mutex<Option<CorridorResult>>>,
    requested: bool,
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

    /// Markers still in front of a position, nearest first.
    pub fn ahead_of(&self, along_m: f64) -> impl Iterator<Item = &AheadItem> {
        self.items.iter().filter(move |i| i.along_m >= along_m)
    }

    /// The next thing you will pass.
    pub fn next_ahead(&self, along_m: f64) -> Option<&AheadItem> {
        self.ahead_of(along_m).next()
    }

    /// Drop everything; the route it described is gone.
    fn reset(&mut self) {
        self.items.clear();
        self.limits.clear();
        self.total_m = 0.0;
        self.status = AheadStatus::Idle;
        self.generation = None;
        self.requested = false;
        self.in_flight = Arc::default();
    }
}

// ─── The system ──────────────────────────────────────────────────────────────

/// Keep [`RouteAhead`] in step with the route being driven: project the saved
/// places immediately, ask Overpass for the corridor once, merge its answer in.
///
/// Gated on driver mode being active. Planning a route deliberately costs nothing
/// on the network beyond the solve itself — the same rule that stops the planner
/// stripping the map (`driver`'s module note) applies to its data appetite.
pub fn drive_route_ahead(
    mut ahead: ResMut<RouteAhead>,
    route: Res<RouteState>,
    driver: Res<DriverState>,
    store: Res<UserStore>,
) {
    if !driver.active {
        if ahead.generation.is_some() {
            ahead.reset();
        }
        return;
    }
    let Some(r) = route.route.as_ref() else {
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
        ahead.total_m = geom.total_m;
        ahead.items = local_items(&geom, &store, &route.to.label);
        ahead.status = AheadStatus::Fetching;

        let slot = ahead.in_flight.clone();
        let corridor = corridor_points(&geom);
        let points = geom.points.clone();
        let cum = geom.cum.clone();
        let total = geom.total_m;
        ahead.requested = true;
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

    // Merge a landed corridor.
    let landed = ahead.in_flight.lock().ok().and_then(|mut g| g.take());
    let Some(landed) = landed else {
        return;
    };
    ahead.requested = false;
    match landed {
        Ok((mut items, limits)) => {
            ahead.limits = limits;
            items.append(&mut ahead.items);
            sort_items(&mut items);
            ahead.items = items;
            ahead.status = AheadStatus::Ready;
            tracing::info!(
                items = ahead.items.len(),
                limited = ahead.limits.iter().filter(|l| l.is_some()).count(),
                "route corridor ready"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "route corridor fetch failed");
            ahead.status = AheadStatus::Failed(e);
        }
    }
}

/// The markers that need no network: the user's own places near the line, and the
/// destination itself.
fn local_items(geom: &RouteGeom, store: &UserStore, destination: &str) -> Vec<AheadItem> {
    let mut out: Vec<AheadItem> = store
        .data
        .bookmarks
        .iter()
        .filter_map(|b| {
            let (along, off) = nearest_on_polyline(&geom.points, &geom.cum, [b.lat, b.lng]);
            (off <= SAVED_NEAR_M).then(|| AheadItem {
                along_m: along,
                off_m: off,
                kind: AheadKind::Saved,
                name: Some(b.name.clone()),
            })
        })
        .collect();
    out.push(AheadItem {
        along_m: geom.total_m,
        off_m: 0.0,
        kind: AheadKind::Destination,
        name: (!destination.trim().is_empty()).then(|| destination.trim().to_string()),
    });
    sort_items(&mut out);
    out
}

/// Order the rail: by distance along, and where two coincide, by what matters
/// more — the rail draws them in this order and the later one loses the pixel.
fn sort_items(items: &mut [AheadItem]) {
    items.sort_by(|a, b| {
        a.along_m
            .total_cmp(&b.along_m)
            .then(b.kind.priority().cmp(&a.kind.priority()))
    });
}

/// The polyline handed to Overpass' `around`, thinned to [`CORRIDOR_STEP_M`].
fn corridor_points(geom: &RouteGeom) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    let mut last_at = f64::NEG_INFINITY;
    for (i, p) in geom.points.iter().enumerate() {
        let at = geom.cum[i];
        if out.is_empty() || at - last_at >= CORRIDOR_STEP_M {
            out.push(*p);
            last_at = at;
        }
    }
    // The end matters as much as the start: without it the corridor stops short of
    // the destination by up to one step, which is where the rail is busiest.
    if let Some(last) = geom.points.last() {
        if out.last() != Some(last) {
            out.push(*last);
        }
    }
    out
}

// ─── The corridor request ────────────────────────────────────────────────────

/// Ask Overpass for everything the rail and the speed sign need, in one request.
///
/// Two output sets in one query rather than two queries: the services want their
/// position (`out center`, so a fuel *way* answers with a point) and the speed
/// limits want geometry (`out geom`, so a way can be matched against the route
/// vertex by vertex). Overpass will emit both from one pass over the corridor,
/// which halves the round-trips and the rate-limit exposure.
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
    let text = match storage::get_string(&key).await {
        Some(cached) => cached,
        None => {
            let fetched = overpass_query(&query).await?;
            // Overpass reports a timeout or a rate-limit as HTTP 200 with a
            // `remark`, so validating before the write is what keeps a wobble from
            // being cached as "there is nothing along this route".
            if let Some(remark) = remark_of(&fetched) {
                return Err(format!("Overpass remark: {remark}"));
            }
            storage::put(&key, fetched.as_bytes()).await;
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
    // Parking is asked for far more narrowly than fuel and charging: `amenity=
    // parking` is also every unnamed rectangle behind a terrace, and a city street
    // has dozens. A car park worth a rail marker is one you could be told to drive
    // to — it has a name, or it is a structure, or it is a park-and-ride.
    format!(
        "[out:json][timeout:90];\
         (\
         nwr(around:{poi},{poly})[\"amenity\"~\"^(fuel|charging_station)$\"];\
         nwr(around:{poi},{poly})[\"amenity\"=\"parking\"][\"name\"];\
         nwr(around:{poi},{poly})[\"amenity\"=\"parking\"]\
         [\"parking\"~\"^(multi-storey|underground|park_and_ride)$\"];\
         nwr(around:{rest},{poly})[\"highway\"~\"^(rest_area|services)$\"];\
         )->.p;\
         way(around:{spd},{poly})[\"highway\"][\"maxspeed\"]->.s;\
         .p out center {max};\
         .s out geom;",
        poi = POI_CORRIDOR_M,
        rest = REST_CORRIDOR_M,
        spd = SPEED_CORRIDOR_M,
        max = MAX_ITEMS * 8,
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

/// Turn one corridor response into rail markers and a speed-limit sample array.
fn parse_corridor(
    text: &str,
    points: &[[f64; 2]],
    cum: &[f64],
    total_m: f64,
) -> CorridorResult {
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
    let mut items: Vec<AheadItem> = Vec::new();

    for el in elements {
        let tags = el.get("tags");
        let tag = |k: &str| tags.and_then(|t| t.get(k)).and_then(|v| v.as_str());

        // A way that came back with geometry *and* a maxspeed is from the speed
        // set; anything else is a service to put on the rail. The two sets are
        // disjoint in practice, and this rule is what keeps them apart if a
        // parking aisle ever carries a limit.
        if let (Some(limit), Some(geometry)) = (
            tag("maxspeed").and_then(parse_maxspeed),
            el.get("geometry").and_then(|g| g.as_array()),
        ) {
            stamp_limit(&mut best, limit, geometry, points, cum);
            continue;
        }

        let Some(kind) = classify(tag("amenity"), tag("highway")) else {
            continue;
        };
        let Some(at) = element_point(el) else {
            continue;
        };
        let (along, off) = nearest_on_polyline(points, cum, at);
        if off > POI_NEAR_M {
            continue;
        }
        items.push(AheadItem {
            along_m: along,
            off_m: off,
            kind,
            name: tag("name").map(str::to_string),
        });
    }

    // Nearest-first, then thin: the rail compresses the far end of a long route to
    // a few pixels, so a cap that dropped near items would be the one thing the
    // widget must not do.
    items.sort_by(|a, b| a.along_m.total_cmp(&b.along_m));
    let mut parking = 0usize;
    items.retain(|i| {
        if i.kind != AheadKind::Parking {
            return true;
        }
        parking += 1;
        parking <= MAX_PARKING
    });
    items.truncate(MAX_ITEMS);
    sort_items(&mut items);

    let limits = best.into_iter().map(|b| b.map(|(l, _)| l)).collect();
    Ok((items, limits))
}

/// Where an Overpass element is: a node's own position, or a way/relation's
/// `center` (which `out center` supplies).
fn element_point(el: &serde_json::Value) -> Option<[f64; 2]> {
    let pair = |v: &serde_json::Value| {
        Some([
            v.get("lat").and_then(serde_json::Value::as_f64)?,
            v.get("lon").and_then(serde_json::Value::as_f64)?,
        ])
    };
    pair(el).or_else(|| el.get("center").and_then(pair))
}

/// Which rail marker an element's tags mean, if any.
fn classify(amenity: Option<&str>, highway: Option<&str>) -> Option<AheadKind> {
    match (amenity, highway) {
        (Some("fuel"), _) => Some(AheadKind::Fuel),
        (Some("charging_station"), _) => Some(AheadKind::Charging),
        (Some("parking"), _) => Some(AheadKind::Parking),
        (_, Some("rest_area" | "services")) => Some(AheadKind::RestArea),
        _ => None,
    }
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
        None => (raw.trim_end_matches("km/h").trim_end_matches("kph").trim(), 1.0),
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
        let points: Vec<[f64; 2]> = (0..=10).map(|i| [53.0 + f64::from(i) * 0.001, 6.0]).collect();
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
        let g = RouteGeom { points: points.clone(), cum, total_m: total, step_along: vec![] };
        let c = corridor_points(&g);
        assert!(c.len() < points.len(), "a 1 km route should thin below 11 points");
        assert_eq!(c.first(), points.first());
        assert_eq!(c.last(), points.last(), "the destination end must survive");
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
            corridor_cache_key(&a.replace("fuel", "bar"))
        );
    }

    /// A dense city centre must not be able to push the one charging point off the
    /// rail with car parks. Regression: a 4.8 km Groningen route returned 45
    /// amenities, 43 of them parking.
    #[test]
    fn parking_cannot_crowd_the_rail() {
        let (points, cum, total) = geom();
        let mut elements: Vec<serde_json::Value> = (0..30)
            .map(|i| {
                serde_json::json!({
                    "type": "node", "id": i, "lat": 53.0 + f64::from(i) * 0.0003, "lon": 6.0,
                    "tags": { "amenity": "parking", "name": format!("P{i}") }
                })
            })
            .collect();
        // One charging point, right at the far end — the last thing a nearest-first
        // truncation would keep.
        elements.push(serde_json::json!({
            "type": "node", "id": 99, "lat": 53.0098, "lon": 6.0,
            "tags": { "amenity": "charging_station" }
        }));
        let text = serde_json::json!({ "elements": elements }).to_string();

        let (items, _) = parse_corridor(&text, &points, &cum, total).expect("parses");
        assert_eq!(
            items.iter().filter(|i| i.kind == AheadKind::Parking).count(),
            MAX_PARKING
        );
        assert!(
            items.iter().any(|i| i.kind == AheadKind::Charging),
            "the charging point must survive a car park flood"
        );
    }

    #[test]
    fn corridor_response_splits_into_services_and_limits() {
        let (points, cum, total) = geom();
        let text = serde_json::json!({
            "elements": [
                { "type": "node", "id": 1, "lat": 53.003, "lon": 6.0005,
                  "tags": { "amenity": "fuel", "name": "Tank" } },
                { "type": "way", "id": 2, "center": { "lat": 53.006, "lon": 6.0 },
                  "tags": { "amenity": "parking" } },
                // Far off the corridor — projected out, not drawn.
                { "type": "node", "id": 3, "lat": 53.003, "lon": 6.02,
                  "tags": { "amenity": "fuel" } },
                { "type": "way", "id": 4,
                  "tags": { "highway": "residential", "maxspeed": "30" },
                  "geometry": [ { "lat": 53.0, "lon": 6.0 }, { "lat": 53.004, "lon": 6.0 } ] },
            ]
        })
        .to_string();

        let (items, limits) = parse_corridor(&text, &points, &cum, total).expect("parses");
        assert_eq!(items.len(), 2, "the far fuel station is out of the corridor");
        assert_eq!(items[0].kind, AheadKind::Fuel);
        assert_eq!(items[0].name.as_deref(), Some("Tank"));
        assert_eq!(items[1].kind, AheadKind::Parking);
        assert!(items[0].along_m < items[1].along_m, "sorted along the route");
        assert_eq!(limits[0], Some(30));
        assert_eq!(limits[20], None, "beyond the tagged way");
    }

    #[test]
    fn an_overpass_remark_is_an_error_not_an_empty_route() {
        let (points, cum, total) = geom();
        let text = r#"{"remark":"runtime error: Query timed out","elements":[]}"#;
        assert!(parse_corridor(text, &points, &cum, total).is_err());
    }

    #[test]
    fn the_destination_sorts_last_and_saved_places_project() {
        let (points, cum, total) = geom();
        let g = RouteGeom { points, cum, total_m: total, step_along: vec![] };
        let mut store = UserStore::load();
        store.data.bookmarks = vec![
            crate::systems::user_store::Bookmark {
                id: 1,
                name: "Near the road".into(),
                lat: 53.005,
                lng: 6.0005,
                altitude: 0.0,
                pose: default(),
                folder: None,
            },
            crate::systems::user_store::Bookmark {
                id: 2,
                name: "Miles away".into(),
                lat: 53.005,
                lng: 6.2,
                altitude: 0.0,
                pose: default(),
                folder: None,
            },
        ];
        let items = local_items(&g, &store, "Grote Markt");
        assert_eq!(items.len(), 2, "only the near place, plus the destination");
        assert_eq!(items[0].kind, AheadKind::Saved);
        assert_eq!(items[0].name.as_deref(), Some("Near the road"));
        assert_eq!(items[1].kind, AheadKind::Destination);
        assert_eq!(items[1].name.as_deref(), Some("Grote Markt"));
        assert!((items[1].along_m - total).abs() < 1.0);
    }

    #[test]
    fn ahead_of_a_position_drops_what_is_behind() {
        let ahead = RouteAhead {
            items: vec![
                AheadItem { along_m: 100.0, off_m: 0.0, kind: AheadKind::Fuel, name: None },
                AheadItem { along_m: 900.0, off_m: 0.0, kind: AheadKind::Parking, name: None },
            ],
            ..default()
        };
        assert_eq!(ahead.ahead_of(0.0).count(), 2);
        assert_eq!(ahead.next_ahead(500.0).map(|i| i.kind), Some(AheadKind::Parking));
        assert!(ahead.next_ahead(1000.0).is_none());
    }
}
