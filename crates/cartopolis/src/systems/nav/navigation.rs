//! The route planner: the Bevy half of ADR-037.
//!
//! [`crate::systems::routing`] owns the graph and the search; this module owns
//! everything around them — fetching the corridor's tiles on a worker, drawing the
//! result in the world, and the planner panel.
//!
//! The panel follows **Organic Maps**' planner rather than a desktop form: two
//! stacked endpoint fields with a swap between them, travel-mode tabs across the
//! top, one summary line (time · distance), and the turn list underneath.
//! Each endpoint can be the current position, a searched place, or a point picked
//! straight off the map — the three ways you actually name a destination.
//!
//! **Why the solve is a thread and not a system.** A cold corridor is up to
//! [`routing::MAX_ROUTE_TILES`] HTTP fetches; even warm it is that many disk reads
//! plus a graph build. All of it is blocking, so it runs on a worker exactly like
//! the tile streamers do, and the panel polls a `Mutex` for the answer. The built
//! graph is cached against its tile list, so flipping between Car/Bike/Walk re-runs
//! only the A* — which is what makes the mode tabs feel instant.

use crate::components::FreeCamera;
use crate::components::RouteVisual;
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use web_time::Instant;

use bevy::asset::RenderAssetUsages;
use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::platform::http;
use crate::resources::{MapAnchor, OsmTileUrlTemplate, RightPanel};
use crate::systems::egui_manager::EguiManager;
use crate::systems::geo_nav::{
    camera_geo, lat_lng_to_world, nominatim_base_url, percent_encode, world_to_lat_lng,
    GeoNavState, GeoSuggestion, Teleport,
};
use crate::systems::globe::GlobeState;
use crate::systems::icons;
use crate::systems::map_stream::MapStream;
use crate::systems::routing::{
    self, format_distance, format_duration, RoadGraph, Route, RouteError, TravelMode,
};
use crate::systems::theme;
use crate::systems::tile_loader::load_tile_image;
use crate::systems::vector_tiles;
use cartopolis_core::TileKey;
use cartopolis_core::{tile_source_kind, TileSourceKind};

/// Typing pause before an endpoint field is geocoded — the search bar's value, so
/// the two feel the same.
const DEBOUNCE: Duration = Duration::from_millis(160);
const MIN_QUERY_LEN: usize = 2;
const MAX_SUGGESTIONS: usize = 6;

/// Height of the route line above the ground plane, in metres.
///
/// The map tile's top face is at −0.01 and the vector surfaces sit between that and
/// 0 (see `vector_tiles::SurfaceGroup::y_offset`); buildings stand on 0. A route
/// drawn *below* the buildings' base would be swallowed wherever it passes an
/// arcade or a bridge pier, so it goes above.
///
/// Half a metre, not the twelve centimetres this first used: with `near = 0` and a
/// 200 km far plane the depth buffer cannot resolve a tenth of a metre at street
/// range, and the line came out shredded into horizontal bands where it lost the
/// fight to the ground beneath it. Physical separation alone still cannot settle
/// that — [`RouteMaterials`]' `depth_bias` is what actually does, exactly as
/// `vector_tiles::SurfaceGroup::depth_bias` settles the surfaces among themselves —
/// but half a metre is a comfortable margin and still reads as painted on the road.
const ROUTE_Y: f32 = 0.5;

/// Half-widths in metres: a bright fill inside a darker casing, the way every road
/// on the basemap is drawn, so the route reads as part of the map rather than an
/// overlay floating on it.
/// A carriageway is 6–10 m, and the line has to stay readable from the few hundred
/// metres up that a route is actually looked at, where one metre is well under a
/// pixel — so it is drawn a shade wider than the street it follows, as Organic Maps
/// does.
const ROUTE_HALF_WIDTH: f32 = 3.2;
const CASING_HALF_WIDTH: f32 = 4.8;

/// Radius of the start/finish discs.
const MARKER_RADIUS: f32 = 7.0;

/// Start and finish, as linear sRGB — **one source for the marker on the map and
/// the glyph in the panel**, which the user sees at the same moment while
/// planning: the coloured dot beside the "Where to?" field is meant to *be* the
/// marker out on the ground. They had drifted to two greens and two reds
/// (`(51,199,89)` vs `(80,190,110)`, `(230,64,56)` vs `(225,95,85)`) 500 lines
/// apart in this file.
///
/// Not palette colours: green-means-start and red-means-finish is wayfinding
/// convention, the same exemption `driver::DRIVE_BLUE` and the road-sign colours
/// claim, and bending them to the app's orange would cost the convention.
const ROUTE_START_RGB: [f32; 3] = [0.20, 0.78, 0.35];
const ROUTE_FINISH_RGB: [f32; 3] = [0.90, 0.25, 0.22];

/// [`ROUTE_START_RGB`] / [`ROUTE_FINISH_RGB`] as an egui colour, for the panel's
/// endpoint glyphs. egui wants sRGB bytes; `Color::srgb` takes the same numbers
/// as floats, so this is only the unit conversion.
fn marker_color(rgb: [f32; 3]) -> egui::Color32 {
    let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgb(byte(rgb[0]), byte(rgb[1]), byte(rgb[2]))
}

/// Half-width of the thin connector drawn from a pin to where the route joins the
/// road, and the gap below which no connector is drawn at all (the pin is on the
/// road already). A long connector — the pin snapped across water to a drivable
/// road — then reads as a thin lead-in, not a fat route line through a canal.
const CONNECTOR_HALF_WIDTH: f32 = 1.0;
const CONNECTOR_MIN_M: f32 = 4.0;

/// Which end of the route a UI action refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    From,
    To,
}

/// One end of the route: what the user typed, what it resolved to, and the
/// in-flight geocoding behind the field.
#[derive(Default)]
pub struct Waypoint {
    /// The field's contents — a place name, an address, or a coordinate pair.
    pub label: String,
    /// Where it resolved to. `None` while the label is unresolved, which is what
    /// keeps a half-typed query from triggering a solve.
    pub coord: Option<(f64, f64)>,
    suggestions: Vec<GeoSuggestion>,
    show_suggestions: bool,
    last_edit: Option<Instant>,
    last_sent: String,
    in_flight: bool,
    results: Arc<Mutex<Option<(String, Result<Vec<GeoSuggestion>, String>)>>>,
}

impl Waypoint {
    /// Pin the waypoint to a resolved place, closing the suggestion popup.
    pub fn set(&mut self, label: impl Into<String>, coord: (f64, f64)) {
        self.label = label.into();
        self.coord = Some(coord);
        self.show_suggestions = false;
        self.suggestions.clear();
        self.last_sent = self.label.clone();
        self.last_edit = None;
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// What the solver is doing, and what the panel shows because of it.
#[derive(Debug, Default, Clone, PartialEq)]
pub enum SolveStatus {
    #[default]
    Idle,
    /// `(tiles fetched, tiles needed)` — a cold corridor takes long enough that a
    /// bare spinner would look hung.
    Fetching(usize, usize),
    Solving,
    Failed(String),
}

/// A solve request and its answer, handed between the panel and the worker.
struct SolveJob {
    seq: u64,
    result: Result<Route, RouteError>,
}

/// The route planner's state.
#[derive(Resource)]
pub struct RouteState {
    pub from: Waypoint,
    pub to: Waypoint,
    pub mode: TravelMode,
    pub route: Option<Route>,
    /// The route the display keeps using while a replacement is being solved.
    ///
    /// Only [`Self::reroute_from`] fills it, and that is the point: an ordinary
    /// edit in the planner *should* take the line away (the destination the user is
    /// looking at is no longer the one drawn), while a reroute should not. Missing
    /// a turn moved the start by a hundred metres and blanked the whole display —
    /// line, distance, ETA, rail — for as long as the corridor took to refetch,
    /// which on a cold cache on a phone is seconds to tens of seconds, against a
    /// `driver::REROUTE_COOLDOWN` of ten. The previous route is still the best
    /// answer anyone has during that window, and it is still *nearly* right: the
    /// vehicle is off it by definition, but everything past the missed turn is
    /// unchanged.
    ///
    /// Cleared by [`Self::invalidate`] (so every other path drops it) and by a
    /// solve that lands. A failed one keeps it, which is what lets driver mode stay
    /// in navigation and offer Retry instead of quietly ending the trip — and it
    /// therefore outlives a trip that is *stopped* on a failed reroute, leaving the
    /// last route driven on the map. That is the rule driver mode already follows
    /// for a completed trip: clearing the line is the planner's act, and a driver
    /// who has just parked may well want to look at what they drove.
    pub held: Option<Route>,
    pub status: SolveStatus,
    /// Endpoint awaiting a click on the map, if the user armed "pick on map".
    pub picking: Option<Endpoint>,
    /// The start is shown as an editable field rather than a one-line footnote.
    /// See [`start_row`] — the destination is the primary field, and the start
    /// only becomes one while it is being changed.
    pub from_editing: bool,
    /// Bumped whenever `route` changes, so the world visuals know to rebuild.
    pub generation: u32,
    /// What the current `route` was solved for; a change here starts a new solve.
    solved_for: Option<((f64, f64), (f64, f64), TravelMode)>,
    /// Monotonic request id, so a stale worker reply is dropped rather than shown.
    seq: u64,
    results: Arc<Mutex<Option<SolveJob>>>,
    progress: Arc<(AtomicUsize, AtomicUsize)>,
    /// The last graph built, keyed by the tile list it was built from. Switching
    /// travel mode reuses it; moving an endpoint usually does not.
    cache: Arc<GraphCache>,
    /// Reverse-geocoded names for map-picked points: `(endpoint, label)`.
    reverse: Arc<Mutex<Vec<(Endpoint, String)>>>,
}

impl Default for RouteState {
    fn default() -> Self {
        Self {
            from: Waypoint::default(),
            to: Waypoint::default(),
            mode: TravelMode::default(),
            route: None,
            held: None,
            status: SolveStatus::Idle,
            picking: None,
            from_editing: false,
            generation: 0,
            solved_for: None,
            seq: 0,
            results: Arc::new(Mutex::new(None)),
            progress: Arc::new((AtomicUsize::new(0), AtomicUsize::new(0))),
            cache: Arc::new(Mutex::new(None)),
            reverse: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// Marks a waypoint label that is still just the coordinates a user picked off
/// the map, rather than a name.
///
/// Load-bearing as a *sentinel*, not decoration: `drive_route_geocoding` renames
/// only a waypoint still wearing it, so a reverse-geocode reply cannot overwrite
/// a name the user chose or a search result already supplied.
///
/// It used to be `"📍 "`, which was the right idea and the wrong character —
/// U+1F4CD was in no font this app loaded, so every map-picked endpoint showed
/// an empty box in front of its coordinates. Latin-1, so it cannot recur; the
/// `no_painted_glyph_is_tofu` test is what caught it. It stays a plain character
/// rather than becoming an icon: this string is *stored* on the waypoint and
/// compared against, so it belongs to the data, not to the chrome.
const PICKED_PREFIX: &str = "· ";

impl RouteState {
    fn waypoint_mut(&mut self, end: Endpoint) -> &mut Waypoint {
        match end {
            Endpoint::From => &mut self.from,
            Endpoint::To => &mut self.to,
        }
    }

    /// Drop the computed route (but not the endpoints), e.g. after an edit.
    fn invalidate(&mut self) {
        if self.route.is_some() {
            self.route = None;
            self.generation = self.generation.wrapping_add(1);
        }
        // A held route outlives an invalidate only when the same call put it there
        // ([`Self::reroute_from`] assigns *after* this runs). Everything else — a
        // cleared endpoint, a swap, Clear — means the drawn line is no longer an
        // answer to any question the user is asking, so it goes.
        self.held = None;
        self.solved_for = None;
    }

    /// The route the display should draw and measure against: the solved one, or
    /// the one being replaced while a reroute is in flight ([`Self::held`]).
    ///
    /// Read by everything that *shows* a route — the world visuals, the driver
    /// chrome, the route rail. Deliberately not read by anything that *edits* one:
    /// the planner's turn list, `frame_route` and the step teleports all still key
    /// off `route`, because a held route is a thing being driven, not a thing being
    /// worked on.
    #[must_use]
    pub fn displayed(&self) -> Option<&Route> {
        self.route.as_ref().or(self.held.as_ref())
    }

    /// Re-solve from a new start, keeping the current route on screen until the
    /// answer lands.
    ///
    /// Two things separate this from a bare `set_endpoint_at(From, …)`, and both
    /// matter to a display that has to keep working through it:
    ///
    /// - the route moves into [`Self::held`] rather than being dropped, so the
    ///   line, the remaining distance and the ETA keep coming from *something*
    ///   instead of falling back to zeros;
    /// - taking it out first means `invalidate` sees no route and so does **not**
    ///   bump the generation — which is exactly what makes `update_route_visuals`
    ///   leave the drawn line alone rather than despawning and rebuilding it. The
    ///   generation still bumps when the solve finishes, so "the reroute landed" is
    ///   as detectable as it was.
    ///
    /// Only the start moves. The destination is what the user asked for, and a
    /// reroute that quietly changed it would be unforgivable.
    pub fn reroute_from(&mut self, lat: f64, lng: f64) {
        let previous = self.route.take();
        self.set_endpoint_at(Endpoint::From, lat, lng);
        self.held = previous;
    }

    /// Ask the panel to resolve this endpoint from a map click.
    fn pick_on_map(&mut self, end: Endpoint) {
        self.picking = Some(end);
        self.waypoint_mut(end).show_suggestions = false;
    }

    /// Set an endpoint to a point on the map: coordinates now, a real address as
    /// soon as the reverse lookup answers.
    ///
    /// Public because a map point can now arrive from two places — the panel's
    /// "pick on map" and the map's own right-click menu ("Route to here") — and
    /// they must behave identically. The context menu doing its own `set` would
    /// silently skip the invalidate and the reverse geocode, leaving a stale line
    /// drawn under a new destination labelled with raw coordinates.
    pub fn set_endpoint_at(&mut self, end: Endpoint, lat: f64, lng: f64) {
        self.picking = None;
        self.waypoint_mut(end)
            .set(format!("{PICKED_PREFIX}{lat:.5}, {lng:.5}"), (lat, lng));
        self.invalidate();
        spawn_reverse(end, lat, lng, self.reverse.clone());
    }

    /// Set an endpoint that already knows what it is called.
    ///
    /// [`Self::set_endpoint_at`] exists for a point picked off the map, where the
    /// only honest label is a coordinate pair until the reverse geocoder answers.
    /// Everything that routes to a *named* thing — a search result, a saved place,
    /// a POI you walked up to — already has the name, and firing a reverse lookup
    /// to rediscover it costs a network round trip to arrive at a worse answer
    /// than the one in hand ("Kerkstraat 12" rather than "Albert Heijn").
    pub fn set_endpoint_named(
        &mut self,
        end: Endpoint,
        label: impl Into<String>,
        lat: f64,
        lng: f64,
    ) {
        self.picking = None;
        self.waypoint_mut(end).set(label.into(), (lat, lng));
        self.invalidate();
    }

    /// Default the start to the user's current position when it is empty.
    ///
    /// "Route to" is the primary act — the destination is what a user has in mind,
    /// and the start is almost always "here". Leaving `from` blank made every route
    /// a two-field form where one field had an obvious answer the app already knew.
    /// Returns whether it filled anything, so callers can invalidate.
    pub fn default_start(&mut self, here: Option<(f64, f64)>) -> bool {
        let Some(here) = here else {
            return false;
        };
        if !self.from.label.is_empty() || self.from.coord.is_some() {
            return false;
        }
        self.from.set(HERE_LABEL, here);
        true
    }
}

/// What the start field says when it is the user's own position. Compared against
/// as well as written, so re-running `default_start` after the camera has moved
/// can refresh the coordinate without stamping over a place the user typed.
pub const HERE_LABEL: &str = "My position";

// ─── Geocoding the endpoint fields ───────────────────────────────────────────

/// Poll finished lookups and fire the debounced next one, for both fields.
///
/// This deliberately mirrors `geo_nav::drive_geocode` rather than sharing it: that
/// one drives a single field wired to the address bar's teleport-on-Enter
/// behaviour, and folding two independent fields with no teleport into it would
/// make the address bar's logic conditional on a mode it does not have.
pub fn drive_route_geocoding(mut route: ResMut<RouteState>) {
    // Names coming back for points the user picked off the map.
    let reverse: Vec<(Endpoint, String)> = route
        .reverse
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    for (end, label) in reverse {
        let wp = route.waypoint_mut(end);
        // Only rename the pin we placed — the user may have typed over it since.
        if wp.coord.is_some() && wp.label.starts_with(PICKED_PREFIX) {
            wp.label = label;
        }
    }

    for end in [Endpoint::From, Endpoint::To] {
        let wp = route.waypoint_mut(end);
        let reply = wp.results.lock().ok().and_then(|mut g| g.take());
        if let Some((query, result)) = reply {
            wp.in_flight = false;
            // Drop a reply the user has already typed past.
            if query == wp.label.trim() {
                match result {
                    Ok(list) => {
                        wp.show_suggestions = !list.is_empty();
                        wp.suggestions = list;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "route endpoint lookup failed");
                        wp.suggestions.clear();
                    }
                }
            }
        }

        let query = wp.label.trim().to_string();
        let due = wp.last_edit.is_some_and(|t| t.elapsed() >= DEBOUNCE);
        if due
            && !wp.in_flight
            && query != wp.last_sent
            && query.len() >= MIN_QUERY_LEN
            && wp.coord.is_none()
        {
            wp.in_flight = true;
            wp.last_edit = None;
            wp.last_sent = query.clone();
            spawn_search(query, wp.results.clone());
        }
    }
}

type SearchSink = Arc<Mutex<Option<(String, Result<Vec<GeoSuggestion>, String>)>>>;

fn spawn_search(query: String, sink: SearchSink) {
    // Detached fetch on the IoTaskPool (ADR-033 seam): wasm-safe, no OS thread.
    IoTaskPool::get()
        .spawn(async move {
            let url = format!(
                "{}/search?format=jsonv2&limit={}&q={}",
                nominatim_base_url(),
                MAX_SUGGESTIONS,
                percent_encode(&query)
            );
            let result = search_fetch(url).await;
            if let Ok(mut g) = sink.lock() {
                *g = Some((query, result));
            }
        })
        .detach();
}

/// Fetch and parse a Nominatim `/search` response for the routing endpoint fields.
async fn search_fetch(url: String) -> Result<Vec<GeoSuggestion>, String> {
    let body = http::fetch_string(http::Request::get(url)).await?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    Ok(json
        .as_array()
        .ok_or("unexpected Nominatim response")?
        .iter()
        .filter_map(GeoSuggestion::from_nominatim)
        .collect())
}

/// Name a map-picked point, so the field reads a place name rather than a pair of
/// decimals. Best-effort: a failure just leaves the coordinates showing.
fn spawn_reverse(end: Endpoint, lat: f64, lng: f64, sink: Arc<Mutex<Vec<(Endpoint, String)>>>) {
    // Detached fetch on the IoTaskPool (ADR-033 seam): wasm-safe, no OS thread.
    IoTaskPool::get()
        .spawn(async move {
            let url = format!(
                "{}/reverse?format=jsonv2&lat={lat}&lon={lng}",
                nominatim_base_url()
            );
            let name = http::fetch_string(http::Request::get(url))
                .await
                .ok()
                .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
                .and_then(|json| {
                    json.get("display_name").and_then(|v| v.as_str()).map(|n| {
                        n.split(',')
                            .map(str::trim)
                            .take(3)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                });
            if let (Some(name), Ok(mut g)) = (name, sink.lock()) {
                g.push((end, name));
            }
        })
        .detach();
}

// ─── Solving ─────────────────────────────────────────────────────────────────

/// How long the graph cache survives with no solve asking for it.
///
/// The cache exists to make *switching travel mode* cost one A* instead of a
/// whole corridor refetch, which is a thing a user does within seconds of seeing
/// a route — not minutes. Past that window it is up to three `Arc<RoadGraph>`
/// built from as many as `MAX_ROUTE_TILES` z14 tiles, i.e. tens of megabytes,
/// pinned for the rest of the session by one route solved once. On a 4 GB phone
/// that sits on top of the clipmap's own residency, so it is worth releasing.
const GRAPH_CACHE_IDLE_S: f32 = 180.0;

/// Start a solve when the endpoints or the mode change, and fold in finished ones.
pub fn drive_route_solver(
    mut route: ResMut<RouteState>,
    tile_url: Res<OsmTileUrlTemplate>,
    time: Res<Time>,
    mut idle_s: Local<f32>,
) {
    // 0. Age the graph cache. Deliberately unconditional — including while a solve
    //    is in flight, because the worker holds its own `Arc` clones and writes the
    //    slot back when it finishes, so releasing it early costs at worst the reuse
    //    this cache was for. Every path that starts or finishes a solve resets the
    //    timer below.
    *idle_s += time.delta_secs();
    if *idle_s > GRAPH_CACHE_IDLE_S {
        *idle_s = 0.0;
        if let Ok(mut g) = route.cache.lock() {
            if g.take().is_some() {
                tracing::debug!(
                    "released the routing graph cache after {GRAPH_CACHE_IDLE_S}s idle"
                );
            }
        }
    }

    // 1. Harvest a finished solve (ignoring one the user has already outrun).
    let done = route.results.lock().ok().and_then(|mut g| g.take());
    if done.is_some() {
        *idle_s = 0.0;
    }
    if let Some(job) = done {
        if job.seq == route.seq {
            match job.result {
                Ok(r) => {
                    tracing::info!(
                        distance_m = r.distance_m,
                        duration_s = r.duration_s,
                        steps = r.steps.len(),
                        mode = ?r.mode,
                        "route solved"
                    );
                    route.route = Some(r);
                    // The replacement is here, so the one the display was holding
                    // on to has done its job.
                    route.held = None;
                    route.status = SolveStatus::Idle;
                }
                Err(e) => {
                    // `held` is deliberately *not* cleared: a failed reroute leaves
                    // the previous route as the only thing anyone can act on, and
                    // driver mode paints the failure over it rather than ending the
                    // trip. An ordinary failed solve has nothing held anyway.
                    route.route = None;
                    route.status = SolveStatus::Failed(e.to_string());
                }
            }
            route.generation = route.generation.wrapping_add(1);
        }
    }

    // 2. Decide whether a new solve is due.
    let (Some(from), Some(to)) = (route.from.coord, route.to.coord) else {
        return;
    };
    let want = (from, to, route.mode);
    if route.solved_for == Some(want) {
        return;
    }
    let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
    if tile_source_kind(&template) != TileSourceKind::Vector {
        // A raster basemap carries no geometry, so there is nothing to route on.
        // Say so rather than spinning forever on an empty graph.
        route.solved_for = Some(want);
        route.status =
            SolveStatus::Failed("Routing needs a vector tile source (CARTO_TILE_URL)".to_string());
        return;
    }

    route.solved_for = Some(want);
    route.seq += 1;
    route.status = SolveStatus::Fetching(0, 0);
    *idle_s = 0.0;
    let seq = route.seq;
    let sink = route.results.clone();
    let progress = route.progress.clone();
    let cache = route.cache.clone();
    let mode = route.mode;
    progress.0.store(0, Ordering::Relaxed);
    progress.1.store(0, Ordering::Relaxed);

    // Two halves on two executors (ADR-033), like every other loader: the corridor
    // fetch is awaited here, and the graph build + A* — which is `add_tile`'s MVT
    // parse per tile plus `finish`'s segment splitting, easily the heaviest CPU
    // job in the client — goes to `platform::workers`. On wasm this future *is* the
    // main thread, so solving a 96-tile route there froze the frame loop outright.
    IoTaskPool::get()
        .spawn(async move {
            match fetch_corridor(from, to, &template, &progress, &cache).await {
                Err(e) => publish_solve(&sink, seq, Err(e)),
                Ok(input) => {
                    crate::platform::workers::submit(Box::new(move || {
                        let result = build_and_search(input, from, to, mode, &cache);
                        publish_solve(&sink, seq, result);
                    }));
                }
            }
        })
        .detach();
}

/// Hand a finished solve to the main thread. Stamped with the request's `seq` so a
/// result the user has already superseded is discarded rather than drawn.
fn publish_solve(sink: &Mutex<Option<SolveJob>>, seq: u64, result: Result<Route, RouteError>) {
    if let Ok(mut g) = sink.lock() {
        *g = Some(SolveJob { seq, result });
    }
}

/// How many corridor tiles are fetched at once.
///
/// The loop this replaces awaited one tile at a time, so a 96-tile route paid 96
/// sequential round trips even natively — on a cold cache that is the whole of the
/// user-visible solve time, and none of it was work. Bounded rather than
/// unlimited for the reason every other streamer is bounded: 96 simultaneous
/// requests is the burst shape that trips the tile server's rate limiting, and
/// this matches `platform::workers`' own ceiling.
const ROUTE_FETCH_CONCURRENCY: usize = 8;

/// The graph or graphs a solve runs on ([`routing::plan_corridor`]).
///
/// Staged carries *three*: one detailed patch per endpoint, and the coarse
/// corridor between them. The two patches cannot share a graph — see
/// `routing::Corridor::Staged`.
#[derive(Clone)]
enum RouteGraphs {
    Direct(Arc<RoadGraph>),
    Staged {
        start: Arc<RoadGraph>,
        end: Arc<RoadGraph>,
        coarse: Arc<RoadGraph>,
    },
}

/// The last solve's graphs, and the corridor they were built from. Keyed by the
/// whole `Corridor` rather than by a tile list, so a staged solve and a direct one
/// over the same ground cannot be mistaken for each other.
type GraphCache = Mutex<Option<(routing::Corridor, RouteGraphs)>>;

/// What the fetch half hands the CPU half.
enum SolveInput {
    /// Same corridor as the last solve — the graphs are reusable as-is. This is
    /// what makes switching travel mode cost one A* instead of a whole refetch.
    Graphs(RouteGraphs),
    /// A fresh corridor: what it is, and the bodies that answered.
    ///
    /// Holding every body at once costs up to `MAX_ROUTE_TILES` × ~500 KB, which is
    /// the "tens of megabytes" that const's own comment is sized around; the
    /// alternative — interleaving parse with fetch — would put the parse back on
    /// the fetching thread, i.e. on the browser's frame loop.
    Bodies {
        corridor: routing::Corridor,
        /// One entry per tier, in the order `plan_corridor` listed them: the
        /// direct corridor alone, or start patch, end patch and coarse corridor.
        tiers: Vec<(u32, Vec<((i64, i64), Arc<Vec<u8>>)>)>,
    },
}

/// Fetch one tier's tiles, reporting into the shared progress counter.
///
/// A free function rather than a closure because it borrows its tile slice into
/// the future it returns, which a closure cannot express.
async fn fetch_tier(
    tiles: &[(i64, i64)],
    zoom: u32,
    template: &str,
    progress: &(AtomicUsize, AtomicUsize),
    done: &mut usize,
) -> Vec<((i64, i64), Arc<Vec<u8>>)> {
    let mut bodies: Vec<((i64, i64), Arc<Vec<u8>>)> = Vec::with_capacity(tiles.len());
    for chunk in tiles.chunks(ROUTE_FETCH_CONCURRENCY) {
        let fetched = futures_util::future::join_all(chunk.iter().map(|tile| async move {
            let key = TileKey::new(tile.0, tile.1, zoom);
            (*tile, key, load_tile_image(&key, template).await)
        }))
        .await;
        for (tile, key, got) in fetched {
            match got {
                Ok(bytes) => bodies.push((tile, bytes)),
                // One missing tile is a hole in the corridor, not a failure: the
                // search may well route around it. All of them missing is a
                // failure, and falls out of the empty-body check in the caller.
                Err(e) => {
                    tracing::warn!(tile = %key.to_path(), error = %e, "route tile fetch failed");
                }
            }
            *done += 1;
            progress.0.store(*done, Ordering::Relaxed);
        }
    }
    bodies
}

/// Whether a cached corridor contains every tile a new one wants, so its graph can
/// be reused whole.
///
/// **This is what makes a reroute cheap.** Missing a turn re-solves from a start a
/// few hundred metres along the route to the same destination, and the capsule that
/// covers is a shorter, narrower version of the one already in the cache — same
/// tiles, minus a few at the tail. Keyed on exact equality alone that is a cache
/// miss and a full corridor refetch: up to `MAX_ROUTE_TILES` requests, seconds to
/// tens of seconds on a cold phone, during which the display has no route. With
/// containment it is one A*.
///
/// Reusing a *superset* is safe in a way reusing an overlapping neighbour would not
/// be: every road the new corridor asked for is present, so the search sees at
/// least as much of the network as it requested. It also sees a little more, which
/// can only improve the answer — `RoadGraph::route` snaps into the routable core of
/// whatever graph it is handed, and a bigger core is a better place to snap into.
///
/// Direct-to-direct only. A staged corridor is three graphs on two lattices whose
/// endpoint patches must stay separate (see `routing::Corridor::Staged`), and
/// "contains" for that is a per-tier question worth answering only if long-haul
/// reroutes ever become a case anyone has.
fn covers(have: &routing::Corridor, want: &routing::Corridor) -> bool {
    let (routing::Corridor::Direct(have), routing::Corridor::Direct(want)) = (have, want) else {
        return false;
    };
    let have: std::collections::HashSet<(i64, i64)> = have.iter().copied().collect();
    want.iter().all(|t| have.contains(t))
}

/// Corridor → tiles → bytes. The I/O half of a solve.
async fn fetch_corridor(
    from: (f64, f64),
    to: (f64, f64),
    template: &str,
    progress: &(AtomicUsize, AtomicUsize),
    cache: &GraphCache,
) -> Result<SolveInput, RouteError> {
    let corridor = routing::plan_corridor(from, to)?;
    let plan: Vec<(u32, Vec<(i64, i64)>)> = match &corridor {
        routing::Corridor::Direct(tiles) => vec![(routing::ROUTING_ZOOM, tiles.clone())],
        routing::Corridor::Staged { start, end, coarse } => vec![
            (routing::ROUTING_ZOOM, start.clone()),
            (routing::ROUTING_ZOOM, end.clone()),
            (routing::LONG_HAUL_ZOOM, coarse.clone()),
        ],
    };
    let total: usize = plan.iter().map(|(_, t)| t.len()).sum();
    progress.1.store(total, Ordering::Relaxed);
    progress.0.store(0, Ordering::Relaxed);

    // Same corridor as last time → skip the fetch entirely and reuse the graphs.
    // Or a corridor *contained* in the last one, which is the reroute case and the
    // reason this second test exists.
    if let Some(graphs) = cache.lock().ok().and_then(|g| {
        g.as_ref()
            .filter(|(c, _)| *c == corridor || covers(c, &corridor))
            .map(|(_, g)| g.clone())
    }) {
        progress.0.store(total, Ordering::Relaxed);
        return Ok(SolveInput::Graphs(graphs));
    }

    let mut done = 0usize;
    let mut tiers = Vec::with_capacity(plan.len());
    for (zoom, tiles) in &plan {
        tiers.push((
            *zoom,
            fetch_tier(tiles, *zoom, template, progress, &mut done).await,
        ));
    }

    if tiers.iter().all(|(_, b)| b.is_empty()) {
        return Err(RouteError::Fetch(format!(
            "none of the {total} map tiles could be read"
        )));
    }
    Ok(SolveInput::Bodies { corridor, tiers })
}

/// Graph → A*. The CPU half of a solve; runs on `platform::workers`.
fn build_and_search(
    input: SolveInput,
    from: (f64, f64),
    to: (f64, f64),
    mode: TravelMode,
    cache: &GraphCache,
) -> Result<Route, RouteError> {
    // One tier's tiles → one graph on that tier's lattice. A graph carries its own
    // zoom (`RoadGraph::new_at`), so nothing downstream has to remember which of
    // the two it is holding.
    let build = |zoom: u32, bodies: Vec<((i64, i64), Arc<Vec<u8>>)>| -> Option<Arc<RoadGraph>> {
        if bodies.is_empty() {
            return None;
        }
        let total = bodies.len();
        let mut graph = RoadGraph::new_at(zoom, (from.0 + to.0) / 2.0);
        let mut failures = 0usize;
        for (tile, bytes) in bodies {
            let key = TileKey::new(tile.0, tile.1, zoom);
            // Shared with the basemap and the geometry cells: a street-level
            // corridor is over ground the client is already streaming, so these
            // bodies are usually parsed already.
            match crate::systems::tile_loader::parse_shared(key, &bytes)
                .and_then(|t| graph.add_tile(&t, tile))
            {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!(tile = %key.to_path(), error = %e, "route tile unreadable");
                    failures += 1;
                }
            }
        }
        if failures == total {
            return None;
        }
        graph.finish();
        tracing::info!(
            zoom,
            tiles = total,
            unreadable = failures,
            nodes = graph.node_count(),
            edges = graph.edge_count(),
            "route graph built"
        );
        Some(Arc::new(graph))
    };

    let graphs = match input {
        SolveInput::Graphs(g) => g,
        SolveInput::Bodies { corridor, tiers } => {
            let mut built = Vec::with_capacity(tiers.len());
            for (zoom, bodies) in tiers {
                let n = bodies.len();
                let Some(g) = build(zoom, bodies) else {
                    return Err(RouteError::Fetch(format!(
                        "none of the {n} map tiles at z{zoom} could be read"
                    )));
                };
                built.push(g);
            }
            let graphs = match built.as_slice() {
                [direct] => RouteGraphs::Direct(direct.clone()),
                [start, end, coarse] => RouteGraphs::Staged {
                    start: start.clone(),
                    end: end.clone(),
                    coarse: coarse.clone(),
                },
                _ => return Err(RouteError::NoData),
            };
            if let Ok(mut g) = cache.lock() {
                *g = Some((corridor, graphs.clone()));
            }
            graphs
        }
    };

    match &graphs {
        RouteGraphs::Direct(g) => g.route(from, to, mode),
        RouteGraphs::Staged { start, end, coarse } => {
            routing::route_staged(start, end, coarse, from, to, mode)
        }
    }
}

// ─── Picking a point off the map ─────────────────────────────────────────────

/// Resolve an armed "pick on map" from the next left click on the ground.
///
/// The camera is the floating origin, so the cursor ray is in render space and the
/// absolute world position is the camera's cell origin plus the hit — the same
/// reconstruction `free_camera` does when it seeds the orbit focus.
pub fn handle_route_map_pick(
    mut route: ResMut<RouteState>,
    egui_manager: Res<EguiManager>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform, &GridCell, &Transform), With<FreeCamera>>,
    map_anchor: Res<MapAnchor>,
) {
    let Some(end) = route.picking else {
        return;
    };
    // Escape disarms — otherwise the next click anywhere would move a pin.
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    if egui_manager.ctx.egui_wants_pointer_input() {
        return;
    }
    let (Ok(window), Ok(anchor)) = (windows.single(), map_anchor.geo.ok_or(())) else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((camera, cam_gt, cell, tf)) = cameras.single() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(cam_gt, cursor) else {
        return;
    };
    let dir = ray.direction.as_vec3().as_dvec3();
    if dir.y > -1e-4 {
        return; // looking at or above the horizon — no ground hit
    }
    // Only the *direction* comes from the ray: its origin is the camera in render
    // space, which is the same point `cell` + `tf` name absolutely — adding both
    // would count the in-cell offset twice.
    let edge = f64::from(WORLD_CELL_EDGE);
    let cam_world = DVec3::new(
        cell.x as f64 * edge + f64::from(tf.translation.x),
        cell.y as f64 * edge + f64::from(tf.translation.y),
        cell.z as f64 * edge + f64::from(tf.translation.z),
    );
    let hit = cam_world + dir * (cam_world.y / -dir.y);
    let (lat, lng) = world_to_lat_lng(anchor, hit.x, hit.z);

    route.set_endpoint_at(end, lat, lng);
    tracing::info!(?end, lat, lng, "route endpoint picked on the map");
}

// ─── World rendering ─────────────────────────────────────────────────────────

/// Materials for the route line, built once.
#[derive(Resource)]
pub struct RouteMaterials {
    fill: Handle<StandardMaterial>,
    casing: Handle<StandardMaterial>,
    start: Handle<StandardMaterial>,
    finish: Handle<StandardMaterial>,
}

/// Rebuild the drawn route when it changes — or when the map re-anchors under it,
/// which moves every world coordinate the line was built from.
#[allow(clippy::too_many_arguments)]
pub fn update_route_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    route: Res<RouteState>,
    stream: Res<MapStream>,
    bigspace: Query<Entity, With<BigSpace>>,
    existing: Query<Entity, With<RouteVisual>>,
    palette: Option<Res<RouteMaterials>>,
    mut drawn: Local<Option<(u32, u32, u32)>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
) {
    let Some(anchor) = stream.geo_anchor() else {
        return;
    };
    // A solved route stays drawn until it is cleared — closing the planner does not
    // erase it. It used to: the line was gated on the Route panel being open, which
    // meant you could not plan a route and then *look* at it, and it tied the drawn
    // route to the same panel state that (wrongly) stripped the map. `Clear` and
    // editing an endpoint both bump `generation`, so the line still goes away the
    // moment the route it draws does.
    // Redrawn when the ground moves under it, too — the line is baked into a mesh.
    let key = (route.generation, stream.anchor_generation, terrain.revision);
    if *drawn == Some(key) {
        return;
    }
    let Ok(root) = bigspace.single() else {
        return;
    };
    *drawn = Some(key);

    for e in existing.iter() {
        commands.entity(e).despawn();
    }
    // `displayed`, not `route`: a reroute in flight has moved the route it is
    // replacing into `held`, and the line must stay on the map while the corridor
    // is refetched. That case does not usually reach here at all — a reroute does
    // not bump the generation, so the key above matches and this system returns
    // early — but a *failed* reroute does bump it, and this is what keeps the line
    // drawn under the failure card instead of erasing it along with the answer.
    let Some(r) = route.displayed() else {
        return;
    };

    // One material set, created on first draw. `unlit` on purpose: the route is a
    // UI affordance drawn into the scene, and it has to stay legible at night, when
    // the streets it follows are lit only by the moon.
    let palette = match palette {
        Some(p) => RouteMaterials {
            fill: p.fill.clone(),
            casing: p.casing.clone(),
            start: p.start.clone(),
            finish: p.finish.clone(),
        },
        None => {
            let mut line = |r: f32, g: f32, b: f32, bias: f32| {
                materials.add(StandardMaterial {
                    base_color: Color::srgb(r, g, b),
                    unlit: true,
                    // Above the ground stack physically as well (see `ROUTE_Y`);
                    // the bias keeps it there at range, where a tenth of a metre is
                    // far below the depth buffer's resolution.
                    depth_bias: bias,
                    // Double-sided: a flat overlay ribbon has no meaningful back, and
                    // this removes triangle winding as a way for the line to vanish
                    // silently — the same "invisible, no error" trap it hit before.
                    cull_mode: None,
                    ..default()
                })
            };
            // Well clear of the ground stack, whose own rungs top out at 10
            // (`vector_tiles::SurfaceGroup::depth_bias`). Matching that range was
            // not enough: the route has to win against the *buildings* and the
            // road surfaces too, and at street range the depth buffer's resolution
            // is coarse enough that a near-tie shows up as banding. The route is a
            // navigation aid — drawing it over the odd building corner is the right
            // trade against losing it under one.
            //
            // These are rungs on the same ladder the surfaces use, so they carry
            // the same `DEPTH_BIAS_STEP` — a bias is denominated in depth-buffer
            // ULPs, and one rung of separation is below the rounding noise.
            let rung = |n: f32| n * vector_tiles::DEPTH_BIAS_STEP;
            let p = RouteMaterials {
                fill: line(0.16, 0.53, 0.90, rung(20.0)),
                casing: line(0.06, 0.22, 0.42, rung(18.0)),
                start: line(
                    ROUTE_START_RGB[0],
                    ROUTE_START_RGB[1],
                    ROUTE_START_RGB[2],
                    rung(22.0),
                ),
                finish: line(
                    ROUTE_FINISH_RGB[0],
                    ROUTE_FINISH_RGB[1],
                    ROUTE_FINISH_RGB[2],
                    rung(22.0),
                ),
            };
            commands.insert_resource(RouteMaterials {
                fill: p.fill.clone(),
                casing: p.casing.clone(),
                start: p.start.clone(),
                finish: p.finish.clone(),
            });
            p
        }
    };

    // `r.points` is `[start pin, road node, …, road node, end pin]`. The pins are
    // where the user aimed; the road nodes are where the route actually joins the
    // network. Drawing the full-width ribbon straight from a pin to the first road
    // node is what put a fat line across a canal when the pin sat on a quay and the
    // nearest *drivable* node was on the far bank — so the road path is drawn at full
    // width and each pin-to-road gap as a thin connector instead.
    let n = r.points.len();
    if n < 2 {
        return;
    }
    let to_world = |p: &[f64; 2]| {
        let (x, z) = lat_lng_to_world(anchor, p[0], p[1]);
        DVec3::new(x, 0.0, z)
    };
    // Road portion is everything between the two pins; with ≥ 2 graph nodes always
    // present (start ≠ goal), that is at least two points.
    let road_world: Vec<DVec3> = r.points[1..n - 1].iter().map(to_world).collect();
    let Some(&origin) = road_world.first() else {
        return;
    };
    let to_local = |w: DVec3| Vec2::new((w.x - origin.x) as f32, (w.z - origin.z) as f32);
    let road: Vec<Vec2> = dedupe(road_world.iter().map(|w| to_local(*w)));
    if road.len() < 2 {
        return;
    }
    let from_pin = to_local(to_world(&r.points[0]));
    let to_pin = to_local(to_world(&r.points[n - 1]));

    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    let ground = terrain.snapshot();
    let mut spawn = |mut mesh: Mesh, material: Handle<StandardMaterial>, at: DVec3| {
        // Follow the measured ground under the road. `ROUTE_Y` still lifts the line
        // clear of the surface stack; the terrain moves that whole stack, and a
        // route drawn on the old flat plane would run through the hill the street
        // goes over.
        crate::systems::terrain::drape_mesh(
            &mut mesh,
            Vec2::new(at.x as f32, at.z as f32),
            &ground,
        );
        let (cell, offset) = grid.translation_to_grid(at);
        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material),
            Transform::from_translation(offset),
            cell,
            ChildOf(root),
            // Flat, big_space-rebased geometry — same reason the map tiles opt out.
            bevy::camera::visibility::NoFrustumCulling,
            RouteVisual,
            // Hidden from orbit with the rest of the ground detail (ADR-025).
            crate::components::HideInSpace,
        ));
    };

    let base = DVec3::new(origin.x, f64::from(ROUTE_Y), origin.z);
    spawn(
        ribbon(&road, CASING_HALF_WIDTH),
        palette.casing.clone(),
        base,
    );
    spawn(ribbon(&road, ROUTE_HALF_WIDTH), palette.fill.clone(), base);

    // Pin-to-road connectors, drawn thin so a long one (the pin snapped across
    // water to reach a drivable road) reads as "get to the road", not as the route.
    // Skipped when the pin already sits on the road, the ordinary case.
    for (pin, road_end, material) in [
        (from_pin, road[0], palette.casing.clone()),
        (to_pin, *road.last().unwrap(), palette.casing.clone()),
    ] {
        if pin.distance(road_end) > CONNECTOR_MIN_M {
            spawn(
                ribbon(&[pin, road_end], CONNECTOR_HALF_WIDTH),
                material,
                base,
            );
        }
    }

    // Start / finish discs, at the coordinates the user actually asked for.
    for (point, material) in [(from_pin, palette.start), (to_pin, palette.finish)] {
        spawn(
            disc(MARKER_RADIUS),
            material,
            base + DVec3::new(f64::from(point.x), 0.01, f64::from(point.y)),
        );
    }
}

/// Merge points closer together than [`MIN_SEGMENT_M`]. The routing graph is noded
/// at ~0.5 m ([`routing`]'s `NODE_TOL`), so a route polyline carries sub-metre
/// segments; a sharp turn on one of those is what made a corner join blow up. At the
/// altitude a route is viewed from, a metre is far below a pixel, so collapsing them
/// costs nothing and removes the degenerate input.
const MIN_SEGMENT_M: f32 = 1.5;

fn dedupe(points: impl Iterator<Item = Vec2>) -> Vec<Vec2> {
    let mut out: Vec<Vec2> = Vec::new();
    for p in points {
        if out
            .last()
            .is_none_or(|last| last.distance_squared(p) > MIN_SEGMENT_M * MIN_SEGMENT_M)
        {
            out.push(p);
        }
    }
    out
}

/// A flat ribbon of constant width along a polyline, in the XZ plane at y = 0.
///
/// **Bevel joins, not mitre.** A mitre offsets each vertex along the angle bisector,
/// lengthened by `1/cos(θ/2)` — which runs to infinity at a sharp turn, and on the
/// sub-metre segments the noded graph produces even a clamped mitre shot a spike
/// clear across a junction (the "giant arrowhead"). Instead each segment is drawn as
/// its own butt-ended quad using only its own perpendicular, and the wedge left at
/// each interior vertex is filled with two corner triangles. That cannot spike (no
/// vertex sits further than `half_width` from the centreline) and cannot pinch (each
/// segment keeps full width end to end), at any angle or segment length. The inner
/// corner triangle overlaps the quads harmlessly — the material is opaque and unlit.
fn ribbon(points: &[Vec2], half_width: f32) -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Append a triangle wound so it faces +Y (up), or it is backface-culled and
    // invisible — the failure `ribbon_faces_up` guards.
    let mut tri = |a: Vec2, b: Vec2, c: Vec2| {
        // Y of (b−a)×(c−a): positive already faces up; otherwise swap b and c.
        let up = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x) < 0.0;
        let (b, c) = if up { (b, c) } else { (c, b) };
        let base = positions.len() as u32;
        for p in [a, b, c] {
            positions.push([p.x, 0.0, p.y]);
        }
        indices.extend([base, base + 1, base + 2]);
    };

    let perp = |d: Vec2| Vec2::new(-d.y, d.x);

    // One butt-ended quad per segment.
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let dir = (b - a).normalize_or_zero();
        if dir == Vec2::ZERO {
            continue;
        }
        let nrm = perp(dir) * half_width;
        tri(a + nrm, b + nrm, a - nrm);
        tri(a - nrm, b + nrm, b - nrm);
    }

    // Fill the wedge at each interior vertex (both sides — the inner one overlaps).
    for i in 1..points.len().saturating_sub(1) {
        let c = points[i];
        let din = (points[i] - points[i - 1]).normalize_or_zero();
        let dout = (points[i + 1] - points[i]).normalize_or_zero();
        if din == Vec2::ZERO || dout == Vec2::ZERO {
            continue;
        }
        let (nin, nout) = (perp(din) * half_width, perp(dout) * half_width);
        tri(c, c + nin, c + nout);
        tri(c, c - nin, c - nout);
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0, 1.0, 0.0]; positions.len()],
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_indices(Indices::U32(indices))
}

/// A flat disc in the XZ plane, for the start/finish markers.
fn disc(radius: f32) -> Mesh {
    const SEGMENTS: usize = 24;
    let mut positions = vec![[0.0f32, 0.0, 0.0]];
    let mut indices = Vec::new();
    for i in 0..SEGMENTS {
        let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        positions.push([radius * a.cos(), 0.0, radius * a.sin()]);
        let next = 1 + (i + 1) % SEGMENTS;
        indices.extend([0u32, next as u32, (i + 1) as u32]);
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0, 1.0, 0.0]; positions.len()],
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_indices(Indices::U32(indices))
}

// ─── The planner panel ───────────────────────────────────────────────────────

/// The route planner, Organic Maps' layout in a docked panel.
#[allow(clippy::too_many_arguments)]
pub fn render_route_panel(
    mut egui: ResMut<EguiManager>,
    mut route: ResMut<RouteState>,
    mut right_panel: ResMut<RightPanel>,
    mut nav: ResMut<GeoNavState>,
    mut driver: ResMut<crate::systems::driver::DriverState>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    map_anchor: Res<MapAnchor>,
) {
    let open_before = right_panel.is(RightPanel::Route);
    if !open_before {
        return;
    }

    // Where "My position" resolves to — the same definition the HUD reads out.
    let here = camera
        .single()
        .ok()
        .zip(map_anchor.geo)
        .map(|((cell, tf), anchor)| camera_geo(anchor, &globe, cell, tf));

    // Fill the start in the moment the panel can see one, so the user only ever
    // has a destination to supply. Only when it is *empty* — refreshing it as the
    // camera drifts would re-invalidate and re-solve the route every frame.
    route.default_start(here);

    // Live fetch progress, for the status line.
    if let SolveStatus::Fetching(..) = route.status {
        let done = route.progress.0.load(Ordering::Relaxed);
        let total = route.progress.1.load(Ordering::Relaxed);
        route.status = if total > 0 && done >= total {
            SolveStatus::Solving
        } else {
            SolveStatus::Fetching(done, total)
        };
    }

    let mut open = true;
    let mut actions = PanelActions::default();
    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let ui_root = egui.ui_mut();

    crate::systems::egui_manager::responsive_right_panel("route_panel", compact, viewport_w, 320.0)
        .show_collapsible(ui_root, &mut open, |ui| {
            let panel = crate::resources::RightPanel::Route;
            actions.close = theme::panel_header(ui, panel.icon(), panel.label());

            // Travel-mode tabs. Organic Maps puts these above the fields, because
            // the mode changes what a sensible destination even is.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for mode in TravelMode::ALL {
                    let selected = route.mode == mode;
                    // Icon *and* word. These were words alone, because the three
                    // emoji the mode used to carry were in no font this app
                    // loaded and the tab read "□ Car"; the bundled subset has a
                    // car, a bike and a walker, and a mode picker is exactly the
                    // control a picture serves — it is read at a glance, over and
                    // over, and the words are short enough to keep beside them.
                    let label = format!("{}  {}", icons::travel_mode(mode), mode.label());
                    if ui
                        .selectable_label(
                            selected,
                            egui::RichText::new(label).size(theme::SIZE_SUB),
                        )
                        .clicked()
                        && !selected
                    {
                        actions.mode = Some(mode);
                    }
                }
            });
            ui.add_space(6.0);

            // The endpoint card. **The destination is the primary field and the
            // start is a footnote**, because "route to somewhere" is the whole act
            // and the start is almost always "here" — which the app already knows.
            // The first cut gave both ends an identical full-width row, which made
            // every route a two-field form where one field had an obvious answer,
            // and left a blank start looking like a required step.
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                start_row(ui, &mut route, here, &mut actions);
                endpoint_row(ui, &mut route, Endpoint::To, here, &mut actions);
            });

            if let Some(end) = route.picking {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(match end {
                        Endpoint::From => "Click the map to set the starting point…",
                        Endpoint::To => "Click the map to set the destination…",
                    })
                    .size(theme::SIZE_SUB)
                    .color(theme::ACCENT_TEXT),
                );
            }

            ui.add_space(8.0);

            // Summary: the one line the planner exists to produce.
            match (&route.status, &route.route) {
                (SolveStatus::Fetching(done, total), _) => {
                    if *total > 0 {
                        theme::busy_progress(ui, "Reading map data…", *done, *total);
                    } else {
                        theme::busy(ui, "Reading map data…");
                    }
                }
                (SolveStatus::Solving, _) => {
                    theme::busy(ui, "Finding the best route…");
                }
                (SolveStatus::Failed(msg), _) => {
                    ui.label(
                        egui::RichText::new(msg.as_str())
                            .size(theme::SIZE_SUB)
                            .color(theme::ERROR),
                    );
                }
                (SolveStatus::Idle, Some(r)) => {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format_duration(r.duration_s))
                                .size(theme::SIZE_HEADING)
                                .strong(),
                        );
                        ui.label(
                            egui::RichText::new(format!("· {}", format_distance(r.distance_m)))
                                .color(theme::TEXT_DIM),
                        );
                    });
                    ui.horizontal(|ui| {
                        // Driving is the thing a solved route is *for*, so it takes
                        // the primary slot. It replaced "Show on map", which framed
                        // the route from above — entering driver mode goes to the
                        // route anyway, and does the rest of it too.
                        if theme::primary_button(ui, "Start driving")
                            .on_hover_text(
                                "Follow this route: strip the map, lock the camera \
                                 behind the arrow, and show the next turn",
                            )
                            .clicked()
                        {
                            actions.drive = true;
                        }
                        if ui.button("Clear").clicked() {
                            actions.clear = true;
                        }
                    });
                }
                (SolveStatus::Idle, None) => {
                    ui.label(
                        egui::RichText::new("Set a start and a destination to plan a route.")
                            .size(theme::SIZE_SUB)
                            .color(theme::TEXT_DIM),
                    );
                }
            }

            // Turn list.
            if let Some(r) = &route.route {
                ui.add_space(6.0);
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, step) in r.steps.iter().enumerate() {
                            let resp = ui
                                .horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(icons::maneuver(step.maneuver))
                                            .size(theme::SIZE_VALUE)
                                            .color(theme::ACCENT_TEXT),
                                    );
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new(step.instruction())
                                                .size(theme::SIZE_SUB),
                                        );
                                        if step.distance_m >= 1.0 {
                                            ui.label(
                                                egui::RichText::new(format_distance(
                                                    step.distance_m,
                                                ))
                                                .size(theme::SIZE_CHIP)
                                                .color(theme::TEXT_DIM),
                                            );
                                        }
                                    });
                                })
                                .response;
                            if resp.interact(egui::Sense::click()).clicked() {
                                actions.goto_step = Some(i);
                            }
                            ui.add_space(3.0);
                        }
                    });
            }
        });

    let drive = actions.drive;
    apply_actions(actions, &mut route, &mut nav, here);
    // Driving takes the whole screen over, so the planner that started it gets out
    // of the way — the turn list is on the manoeuvre card now.
    if drive && route.route.is_some() {
        driver.enter();
        *right_panel = RightPanel::None;
        return;
    }
    if !open {
        actions_close(&mut right_panel);
    }
}

fn actions_close(right_panel: &mut RightPanel) {
    *right_panel = RightPanel::None;
}

/// What the panel's widgets asked for this frame. Collected rather than applied
/// inline because the widgets borrow `route` while they draw.
#[derive(Default)]
struct PanelActions {
    close: bool,
    swap: bool,
    clear: bool,
    frame: bool,
    /// Start driving the solved route (`systems::driver`).
    drive: bool,
    mode: Option<TravelMode>,
    goto_step: Option<usize>,
    /// Endpoint set to the current position.
    use_here: Option<Endpoint>,
    /// Endpoint armed for a map click.
    pick: Option<Endpoint>,
    /// Endpoint resolved from a suggestion.
    chose: Option<(Endpoint, GeoSuggestion)>,
    /// Endpoint whose text changed (invalidates its coordinate).
    edited: Option<Endpoint>,
    cleared: Option<Endpoint>,
    /// Open the start as an editable field / fold it back to a footnote.
    edit_start: bool,
    collapse_start: bool,
}

/// The start, as a footnote above the destination.
///
/// It is a full endpoint row only while it is being *changed*: at rest it is one
/// muted line reading where the route starts, because the answer is nearly always
/// "here" and a text field invites filling in something that is already correct.
/// Clicking it opens the field; the pin sets it back to the current position and
/// the swap sends the current destination up here, which is how you say "route
/// *from* that place" without retyping it.
fn start_row(
    ui: &mut egui::Ui,
    route: &mut RouteState,
    here: Option<(f64, f64)>,
    actions: &mut PanelActions,
) {
    let has_here = here.is_some();
    // Editing sticks while the field has focus or the label is still unresolved —
    // otherwise the row would snap shut mid-word the frame a suggestion arrived.
    let editing = route.from_editing || route.from.coord.is_none();
    if editing {
        endpoint_row(ui, route, Endpoint::From, here, actions);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            if theme::action_button(ui, icons::SWAP)
                .on_hover_text("Swap start and destination")
                .clicked()
            {
                actions.swap = true;
            }
            if route.from.coord.is_some()
                && theme::action_button(ui, "Done")
                    .on_hover_text("Collapse")
                    .clicked()
            {
                actions.collapse_start = true;
            }
        });
        return;
    }

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.label(
            egui::RichText::new("From")
                .size(theme::SIZE_CHIP)
                .color(theme::TEXT_DIM),
        );
        let label = if route.from.label == HERE_LABEL {
            // Say what it *means*, not what it is called: "My position" in a field
            // is a value, but as a footnote it reads better as the default it is.
            format!("{} {}", icons::HERE, HERE_LABEL)
        } else {
            route.from.label.clone()
        };
        if ui
            .add(
                egui::Label::new(
                    egui::RichText::new(label)
                        .size(theme::SIZE_SUB)
                        .color(theme::TEXT),
                )
                .truncate()
                .sense(egui::Sense::click()),
            )
            .on_hover_text("Click to change the starting point")
            .clicked()
        {
            actions.edit_start = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::action_button(ui, icons::SWAP)
                .on_hover_text("Swap start and destination")
                .clicked()
            {
                actions.swap = true;
            }
            if theme::action_enabled_because(
                ui,
                has_here,
                icons::HERE,
                "No position yet — the map has not placed you",
            )
            .on_hover_text("Start from my position")
            .clicked()
            {
                actions.use_here = Some(Endpoint::From);
            }
        });
    });
}

/// Points the endpoint row keeps clear to the right of the field, for the
/// actions that live there: clear, pick-on-map and use-my-position, plus the
/// spacing between them. Sized for the touch tier's buttons, which is the tier
/// that cannot afford to be wrong — a pointer simply gets a slightly shorter
/// field than it strictly needs.
const WAYPOINT_ACTIONS_PTS: f32 = 108.0;
/// Longest the field is allowed to get. Past this a wide window is spending
/// screen on a place name that is almost always short, and the row stops
/// reading as one control with its shortcuts beside it.
const WAYPOINT_FIELD_MAX_PTS: f32 = 180.0;

/// How wide the "Where to?" field may be, given what is left of the row.
///
/// It used to be a flat 180 pt, which on a compact panel (a phone's whole
/// viewport, ~240 pt inside the margins) left about 40 pt for three buttons that
/// need a hundred — so the actions that let you *avoid typing an address on a
/// phone keyboard* were the first things off the edge, on the one device where
/// they matter most.
///
/// The floor is what keeps this honest on an absurdly narrow window: the field
/// stops shrinking and the row overflows, which is visible, rather than the
/// field collapsing to nothing while everything still fits.
#[must_use]
fn waypoint_field_width(available: f32) -> f32 {
    (available - WAYPOINT_ACTIONS_PTS).clamp(72.0, WAYPOINT_FIELD_MAX_PTS)
}

/// One endpoint: the icon, the text field with its suggestion popup, and the two
/// shortcuts that avoid typing at all.
fn endpoint_row(
    ui: &mut egui::Ui,
    route: &mut RouteState,
    end: Endpoint,
    // Where the viewer is: enables the "use my position" shortcut, and gives the
    // suggestion rows the distance they quote.
    here: Option<(f64, f64)>,
    actions: &mut PanelActions,
) {
    let has_here = here.is_some();
    let (glyph, colour, hint) = match end {
        Endpoint::From => (icons::START, marker_color(ROUTE_START_RGB), "Start"),
        // Phrased as the question rather than as a field name: this is the one
        // thing the panel asks for, and "Destination" reads like a form label.
        Endpoint::To => (icons::FINISH, marker_color(ROUTE_FINISH_RGB), "Where to?"),
    };
    let wp = match end {
        Endpoint::From => &mut route.from,
        Endpoint::To => &mut route.to,
    };

    let mut field_rect = egui::Rect::NOTHING;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(glyph)
                .size(theme::SIZE_SUB)
                .color(colour),
        );
        let field = crate::systems::text_touch::field(
            ui,
            egui::TextEdit::singleline(&mut wp.label)
                .desired_width(waypoint_field_width(ui.available_width()))
                .hint_text(hint),
        );
        field_rect = field.rect;
        if field.changed() {
            wp.coord = None;
            wp.last_edit = Some(Instant::now());
            actions.edited = Some(end);
        }
        if wp.in_flight {
            ui.spinner();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !wp.label.is_empty() && theme::action_button(ui, icons::CLEAR).clicked() {
                actions.cleared = Some(end);
            }
            if theme::action_button(ui, icons::PICK_ON_MAP)
                .on_hover_text("Pick this point on the map")
                .clicked()
            {
                actions.pick = Some(end);
            }
            if theme::action_enabled_because(
                ui,
                has_here,
                icons::HERE,
                "No position yet — the map has not placed you",
            )
            .on_hover_text("Use my position")
            .clicked()
            {
                actions.use_here = Some(end);
            }
        });
    });

    if !wp.show_suggestions || wp.suggestions.is_empty() {
        return;
    }
    let mut picked = None;
    // Clamp to the viewport, both the width and the left edge. The old
    // `field_rect.width().max(300.0)` was an unconditional 300 pt floor, and a
    // phone's egui viewport is 274 pt wide — so the one dropdown a routing user
    // reads most hung 26 pt off the right edge, on the platform this release is
    // for. `geo_nav`'s equivalent popup already does exactly this; the two are
    // the same control in two panels and had drifted apart.
    let screen = ui.ctx().viewport_rect();
    let popup_width = field_rect
        .width()
        .max(300.0)
        .min((screen.width() - 32.0).max(160.0));
    let mut popup_pos = field_rect.left_bottom() + egui::vec2(0.0, 2.0);
    popup_pos.x = popup_pos
        .x
        .min(screen.right() - popup_width - 16.0)
        .max(screen.left() + 16.0);
    egui::Area::new(egui::Id::new(("route_suggestions", hint)))
        .order(egui::Order::Foreground)
        .fixed_pos(popup_pos)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                // Same rows as the address bar's dropdown — name, what kind of
                // place it is, the address context and how far away it is.
                let width = popup_width;
                ui.set_min_width(width);
                ui.set_max_width(width);
                for (i, s) in wp.suggestions.iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    if crate::systems::geo_nav::suggestion_row(ui, s, here, width).clicked() {
                        picked = Some(s.clone());
                    }
                }
            });
        });
    if let Some(s) = picked {
        actions.chose = Some((end, s));
    }
}

/// Apply everything the panel asked for, now that the borrows are released.
fn apply_actions(
    actions: PanelActions,
    route: &mut RouteState,
    nav: &mut GeoNavState,
    here: Option<(f64, f64)>,
) {
    if let Some(mode) = actions.mode {
        route.mode = mode;
        // Only the search re-runs — `solve` reuses the cached graph.
        route.solved_for = None;
        route.status = SolveStatus::Idle;
    }
    if let Some(end) = actions.edited {
        let _ = end;
        route.invalidate();
    }
    if let Some(end) = actions.cleared {
        route.waypoint_mut(end).clear();
        route.invalidate();
        route.status = SolveStatus::Idle;
    }
    if let Some((end, s)) = actions.chose {
        route.waypoint_mut(end).set(s.label, (s.lat, s.lng));
        route.invalidate();
    }
    if let (Some(end), Some(here)) = (actions.use_here, here) {
        route.waypoint_mut(end).set(HERE_LABEL, here);
        route.invalidate();
        // Setting the start to "here" is the one thing the footnote form does
        // best; there is nothing left to type, so fold it back.
        if end == Endpoint::From {
            route.from_editing = false;
        }
    }
    if let Some(end) = actions.pick {
        route.pick_on_map(end);
    }
    if actions.swap {
        std::mem::swap(&mut route.from, &mut route.to);
        route.invalidate();
    }
    if actions.edit_start {
        route.from_editing = true;
    }
    if actions.collapse_start {
        route.from_editing = false;
    }
    if actions.clear {
        route.from.clear();
        route.to.clear();
        route.from_editing = false;
        route.invalidate();
        route.status = SolveStatus::Idle;
    }
    if actions.frame {
        if let Some(tp) = frame_route(route) {
            nav.pending_teleport = Some(tp);
        }
    }
    if let Some(i) = actions.goto_step {
        if let Some(step) = route.route.as_ref().and_then(|r| r.steps.get(i)) {
            nav.pending_teleport = Some(Teleport::geo(step.at[0], step.at[1], Some(220.0)));
        }
    }
}

/// A teleport that frames the whole route: its centre, at an altitude that fits the
/// longer span on screen.
fn frame_route(route: &RouteState) -> Option<Teleport> {
    let r = route.route.as_ref()?;
    let (mut min_lat, mut max_lat) = (f64::MAX, f64::MIN);
    let (mut min_lng, mut max_lng) = (f64::MAX, f64::MIN);
    for p in &r.points {
        min_lat = min_lat.min(p[0]);
        max_lat = max_lat.max(p[0]);
        min_lng = min_lng.min(p[1]);
        max_lng = max_lng.max(p[1]);
    }
    let mid_lat = (min_lat + max_lat) / 2.0;
    // Degrees → metres, with longitude foreshortened by the latitude.
    let span_m = ((max_lat - min_lat) * 111_320.0)
        .max((max_lng - min_lng) * 111_320.0 * mid_lat.to_radians().cos());
    Some(Teleport::geo(
        mid_lat,
        (min_lng + max_lng) / 2.0,
        // A ~60° vertical field of view frames a span from roughly its own length
        // away; the margin keeps the endpoints off the screen edge.
        Some((span_m * 0.9).clamp(200.0, 60_000.0)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The endpoint field yields to the actions beside it rather than pushing
    /// them off the edge. On a phone's whole viewport (~240 pt inside the panel
    /// margins) the flat 180 pt this replaced left about 40 pt for three buttons
    /// that need a hundred — so on the one device where "pick it on the map" and
    /// "use my position" matter most, they were the first things to go.
    #[test]
    fn the_endpoint_field_leaves_room_for_its_actions() {
        for row in [200.0_f32, 240.0, 260.0] {
            let field = waypoint_field_width(row);
            assert!(
                row - field >= WAYPOINT_ACTIONS_PTS - 0.01,
                "a {row} pt row left only {} pt for the actions",
                row - field
            );
        }
        // A wide window stops growing the field: past this the row stops reading
        // as one control with its shortcuts beside it.
        assert_eq!(waypoint_field_width(2000.0), WAYPOINT_FIELD_MAX_PTS);
        // And an absurdly narrow one overflows visibly instead of collapsing the
        // field to nothing.
        assert!(waypoint_field_width(40.0) >= 72.0);
    }

    /// The ribbon must be a proper triangle mesh whose width matches the request —
    /// the route is unusable if the line is inverted, degenerate, or hairline.
    #[test]
    fn ribbon_has_the_requested_width() {
        let points = vec![Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0)];
        let mesh = ribbon(&points, 3.0);
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
            bevy::render::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => panic!("positions are not Float32x3"),
        };
        // A due-east segment is offset purely in z, by the half-width each way.
        let max_z = positions.iter().map(|p| p[2]).fold(f32::MIN, f32::max);
        let min_z = positions.iter().map(|p| p[2]).fold(f32::MAX, f32::min);
        assert!(
            (max_z - 3.0).abs() < 1e-3 && (min_z + 3.0).abs() < 1e-3,
            "width off: {min_z}..{max_z}"
        );
        assert!(positions.iter().all(|p| p[1] == 0.0), "ribbon must be flat");

        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("expected u32 indices")
        };
        assert!(!indices.is_empty() && indices.len() % 3 == 0);
        assert!(indices.iter().all(|&i| (i as usize) < positions.len()));
    }

    /// A downward-wound ribbon is backface-culled: no line on the map, no error
    /// anywhere. This is the test for the bug that shipped in the first draft.
    #[test]
    fn ribbon_faces_up() {
        let points = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(100.0, 0.0),
            Vec2::new(100.0, 80.0),
        ];
        let mesh = ribbon(&points, 3.0);
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
            bevy::render::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => panic!("positions are not Float32x3"),
        };
        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("expected u32 indices")
        };
        for t in indices.chunks_exact(3) {
            let (a, b, c) = (
                positions[t[0] as usize],
                positions[t[1] as usize],
                positions[t[2] as usize],
            );
            // Y component of (b−a)×(c−a): positive means the triangle faces up.
            let cross = (b[2] - a[2]) * (c[0] - a[0]) - (b[0] - a[0]) * (c[2] - a[2]);
            assert!(cross > 0.0, "triangle {t:?} wound downward (cross {cross})");
        }
    }

    /// The bevel join must never spike, whatever the turn: no ribbon vertex may sit
    /// further than `half_width` from the centreline. This is the regression for the
    /// "giant arrowhead" — a mitre offset shooting across a junction. A near-hairpin
    /// on a *short* segment (the exact case the noded graph produces) is the worst
    /// case, so use one.
    #[test]
    fn bevel_join_never_spikes() {
        const HW: f32 = 3.0;
        let points = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(100.0, 0.0),
            Vec2::new(100.8, 0.3), // a 0.85 m segment doubling back ~150°
            Vec2::new(0.0, 5.0),
        ];
        let mesh = ribbon(&points, HW);
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
            bevy::render::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => panic!("positions are not Float32x3"),
        };
        // Every vertex must lie within half_width of the nearest input point (the
        // bevel offsets from vertices only), give or take rounding.
        for p in &positions {
            let v = Vec2::new(p[0], p[2]);
            let nearest = points
                .iter()
                .map(|q| q.distance(v))
                .fold(f32::MAX, f32::min);
            assert!(
                nearest <= HW + 1e-3,
                "spike: vertex {v:?} is {nearest:.2} m from the centreline (half_width {HW})"
            );
        }
    }

    /// The whole path end to end — corridor, real tile fetches, graph, A* — against
    /// a live vector source. Not part of the suite: it needs the network (or a warm
    /// `cache/tiles`) and a `CARTO_TILE_URL`, so it is the manual check for "does
    /// routing actually work against the tiles we ship", the way `vector_tiles`'
    /// `dump_png` is the manual check for the styling.
    ///
    /// ```text
    /// CARTO_TILE_URL=https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y} \
    ///   cargo test -p cartopolis solves_a_real_route -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn solves_a_real_route() {
        let template = std::env::var("CARTO_TILE_URL")
            .unwrap_or_else(|_| "https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}".to_string());
        // Two points ~1 km apart in the fixture city, by coordinate.
        let from = (53.2194, 6.5665);
        let to = (53.2109, 6.5642);
        let progress = (AtomicUsize::new(0), AtomicUsize::new(0));
        let cache = Mutex::new(None);

        for mode in TravelMode::ALL {
            let t0 = Instant::now();
            // The two halves the live solve runs on two executors: the corridor
            // fetch on the `IoTaskPool`, the graph build + A* on a worker. Driving
            // them back to back here is the same sequence, minus the hand-off.
            let route = futures_lite::future::block_on(fetch_corridor(
                from, to, &template, &progress, &cache,
            ))
            .and_then(|input| build_and_search(input, from, to, mode, &cache))
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
            println!(
                "{:<5} {:>8} {:>10} in {:>5} ms  ({} steps, {} tiles)",
                mode.label(),
                format_duration(route.duration_s),
                format_distance(route.distance_m),
                t0.elapsed().as_millis(),
                route.steps.len(),
                progress.1.load(Ordering::Relaxed),
            );
            for step in &route.steps {
                println!(
                    "    {:<14} {:<52} {}",
                    format!("{:?}", step.maneuver),
                    step.instruction(),
                    format_distance(step.distance_m)
                );
            }
            // These two points are about 1.2 km apart on foot; nothing sane is
            // under 800 m or over 4 km.
            assert!(
                (800.0..4_000.0).contains(&route.distance_m),
                "{mode:?}: implausible distance {:.0} m",
                route.distance_m
            );
        }
    }

    /// A bench for the *cost model*, against live tiles. Prints distance, time and
    /// the implied average speed for a spread of real corridors, which is the only
    /// way to tell whether a change to the speed table or the junction delay left
    /// city driving plausible — a route that is merely *a* route tells you nothing.
    ///
    /// Not an assertion beyond a sanity band: the right answer is a judgement about
    /// a city, so this exists to be read.
    ///
    /// ```text
    /// cargo test -p cartopolis cost_model_bench -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn cost_model_bench() {
        let template = std::env::var("CARTO_TILE_URL")
            .unwrap_or_else(|_| "https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}".to_string());
        // Groningen: across the centre, centre→ring road, and a long haul out to
        // the motorway — a residential-grid case, a mixed case and a case that
        // should end up on the trunk network.
        let pairs: [(&str, (f64, f64), (f64, f64)); 7] = [
            ("centre → station", (53.2194, 6.5665), (53.2109, 6.5642)),
            ("centre → Paddepoel", (53.2194, 6.5665), (53.2405, 6.5455)),
            ("centre → Ring West", (53.2194, 6.5665), (53.2230, 6.5170)),
            ("centre → Hoogkerk", (53.2194, 6.5665), (53.2100, 6.4700)),
            // Across the Westerschelde: only the ferry connects these, so a walk
            // or a bike route existing at all is the ferry layer working, and the
            // car having none is the deliberate refusal (`RoadKind::Ferry`).
            (
                "Vlissingen → Breskens",
                (51.4450, 3.5700),
                (51.3990, 3.5570),
            ),
            // Past street-level reach: these take `plan_corridor`'s staged path —
            // coarse haul, detailed ends. The snap column is the one to read,
            // because driver mode calls anything past 60 m off-route, and a
            // coarse-only long route snaps ~281 m out.
            (
                "Groningen → Assen (26 km)",
                (53.2194, 6.5665),
                (52.9950, 6.5620),
            ),
            (
                "Groningen → Amsterdam (180 km)",
                (53.2194, 6.5665),
                (52.3730, 4.8920),
            ),
        ];
        for (name, from, to) in pairs {
            let progress = (AtomicUsize::new(0), AtomicUsize::new(0));
            let cache = Mutex::new(None);
            for mode in TravelMode::ALL {
                let input = futures_lite::future::block_on(fetch_corridor(
                    from, to, &template, &progress, &cache,
                ));
                // Timed from here, so the figure is the graph build plus the
                // search rather than the network — the second and third mode of a
                // pair reuse the cached graph and so time the search alone.
                let t0 = Instant::now();
                let route = input.and_then(|i| build_and_search(i, from, to, mode, &cache));
                let solve = t0.elapsed();
                match route {
                    Ok(r) => {
                        // Distance from each pin to the road the route actually
                        // starts and ends on — driver mode's off-route threshold
                        // is 60 m, so this column is a pass/fail.
                        let snap = |pin: [f64; 2], road: [f64; 2]| {
                            let mid = (pin[0] + road[0]) * 0.5;
                            let dx = (road[1] - pin[1]) * 111_320.0 * mid.to_radians().cos();
                            let dy = (road[0] - pin[0]) * 110_540.0;
                            dx.hypot(dy)
                        };
                        let last = r.points.len() - 1;
                        println!(
                            "{name:<32} {:<5} {:>8} {:>10}  {:>5.1} km/h  snap {:>4.0}/{:<4.0} m  \
                             ({} steps, {} tiles, {} ms)",
                            mode.label(),
                            format_distance(r.distance_m),
                            format_duration(r.duration_s),
                            r.distance_m / r.duration_s * 3.6,
                            snap(r.points[0], r.points[1]),
                            snap(r.points[last], r.points[last - 1]),
                            r.steps.len(),
                            progress.1.load(Ordering::Relaxed),
                            solve.as_millis(),
                        );
                    }
                    Err(e) => println!("{name:<32} {:<5} FAILED: {e}", mode.label()),
                }
            }
            println!();
        }
    }

    /// What a *coarse* graph can actually do, at each zoom, on a route far beyond
    /// street-level reach. The question this answers is whether a long haul needs
    /// the fine network stitched onto its ends at all, or whether the generalised
    /// hierarchy routes end to end on its own.
    ///
    /// ```text
    /// cargo test -p cartopolis coarse_zoom_probe -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn coarse_zoom_probe() {
        let template = std::env::var("CARTO_TILE_URL")
            .unwrap_or_else(|_| "https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}".to_string());
        let cases: [(&str, (f64, f64), (f64, f64)); 3] = [
            (
                "Groningen → Assen (26 km)",
                (53.2194, 6.5665),
                (52.9950, 6.5620),
            ),
            (
                "Groningen → Zwolle (95 km)",
                (53.2194, 6.5665),
                (52.5168, 6.0830),
            ),
            (
                "Groningen → Amsterdam (180 km)",
                (53.2194, 6.5665),
                (52.3730, 4.8920),
            ),
        ];
        for (name, from, to) in cases {
            println!("── {name}");
            for zoom in [14u32, 12, 10] {
                let tiles =
                    match routing::corridor_tiles_at(zoom, from, to, routing::MAX_ROUTE_TILES) {
                        Ok(t) => t,
                        Err(e) => {
                            println!("   z{zoom:<2} corridor refused: {e}");
                            continue;
                        }
                    };
                let t0 = Instant::now();
                let mut bytes = 0usize;
                let mut graph = RoadGraph::new_at(zoom, (from.0 + to.0) / 2.0);
                for tile in &tiles {
                    let key = TileKey::new(tile.0, tile.1, zoom);
                    let Ok(body) = futures_lite::future::block_on(load_tile_image(&key, &template))
                    else {
                        continue;
                    };
                    bytes += body.len();
                    if let Ok(td) = vector_tiles::TileData::parse(&body) {
                        let _ = graph.add_tile(&td, *tile);
                    }
                }
                graph.finish();
                let fetched = t0.elapsed();
                let t1 = Instant::now();
                let route = graph.route(from, to, TravelMode::Car);
                // How far the pins are from the road the route actually starts
                // and ends on. This is the number driver mode lives or dies by:
                // its off-route threshold is 60 m, so a coarse snap further out
                // than that puts a driver off-route on the first frame.
                let snap = |pin: [f64; 2], road: [f64; 2]| {
                    let mid = (pin[0] + road[0]) * 0.5;
                    let dx = (road[1] - pin[1]) * 111_320.0 * mid.to_radians().cos();
                    let dy = (road[0] - pin[0]) * 110_540.0;
                    dx.hypot(dy)
                };
                match route {
                    Ok(r) => println!(
                        "   z{zoom:<2} {:>3} tiles {:>6.1} MB  {:>8} {:>10}  {:>5.1} km/h  \
                         {:>3} steps  snap {:>5.0}/{:<5.0} m  (fetch+build {:>5} ms, search {:>4} ms)",
                        tiles.len(),
                        bytes as f64 / 1024.0 / 1024.0,
                        format_distance(r.distance_m),
                        format_duration(r.duration_s),
                        r.distance_m / r.duration_s * 3.6,
                        r.steps.len(),
                        snap(r.points[0], r.points[1]),
                        snap(r.points[r.points.len() - 1], r.points[r.points.len() - 2]),
                        fetched.as_millis(),
                        t1.elapsed().as_millis(),
                    ),
                    Err(e) => println!(
                        "   z{zoom:<2} {:>3} tiles {:>6.1} MB  FAILED: {e}",
                        tiles.len(),
                        bytes as f64 / 1024.0 / 1024.0
                    ),
                }
            }
        }
    }

    #[test]
    fn dedupe_merges_sub_segment_points() {
        // Exact duplicates and points closer than MIN_SEGMENT_M collapse; points
        // further apart survive. Here: a cluster near the origin (all within the
        // threshold of each other) → one point, then a distant one → two total.
        let pts = dedupe(
            [
                Vec2::new(0.0, 0.0),
                Vec2::new(0.0, 0.0),
                Vec2::new(MIN_SEGMENT_M * 0.5, 0.0), // sub-segment: merges
                Vec2::new(50.0, 0.0),                // far: kept
            ]
            .into_iter(),
        );
        assert_eq!(pts.len(), 2, "got {pts:?}");
        assert_eq!(pts[0], Vec2::new(0.0, 0.0));
        assert_eq!(pts[1], Vec2::new(50.0, 0.0));
    }

    #[test]
    fn a_disc_is_closed_and_flat() {
        let mesh = disc(4.0);
        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("expected u32 indices")
        };
        assert_eq!(indices.len() % 3, 0);
        // Every rim vertex is used, so the disc has no gap in it.
        let used: std::collections::HashSet<u32> = indices.iter().copied().collect();
        assert_eq!(used.len(), 25, "centre plus 24 rim vertices");
    }

    /// Editing a field must drop the coordinate, so a half-typed name can never be
    /// solved against the place the user has just typed over.
    #[test]
    fn editing_an_endpoint_invalidates_it() {
        let mut route = RouteState::default();
        route.from.set("Start", (53.2194, 6.5665));
        route.to.set("Destination", (53.2110, 6.5645));
        assert!(route.from.coord.is_some());

        route.solved_for = Some(((53.2194, 6.5665), (53.2110, 6.5645), TravelMode::Car));
        route.invalidate();
        assert_eq!(route.solved_for, None, "a new solve must be due");
    }

    /// The framing altitude has to scale with the route, and stay inside the band
    /// where the map tier still streams detail.
    #[test]
    fn framing_scales_with_the_route() {
        let mut route = RouteState::default();
        let mk = |span: f64| Route {
            mode: TravelMode::Car,
            points: vec![[53.20, 6.55], [53.20 + span, 6.55]],
            distance_m: span * 111_320.0,
            duration_s: 60.0,
            steps: Vec::new(),
        };

        route.route = Some(mk(0.001));
        let short = frame_route(&route).unwrap().altitude.unwrap();
        route.route = Some(mk(0.1));
        let long = frame_route(&route).unwrap().altitude.unwrap();

        assert!(long > short, "a longer route must be framed from higher up");
        assert!((200.0..=60_000.0).contains(&short));
        assert!((200.0..=60_000.0).contains(&long));
    }

    /// "Route to" is the primary act: the start defaults to where you are, so the
    /// user only ever supplies a destination.
    #[test]
    fn the_start_defaults_to_the_current_position() {
        let mut route = RouteState::default();
        assert!(route.default_start(Some((53.22, 6.57))));
        assert_eq!(route.from.label, HERE_LABEL);
        assert_eq!(route.from.coord, Some((53.22, 6.57)));
    }

    /// `set_endpoint_at` fires the reverse geocode on the `IoTaskPool`, which a
    /// unit test has no Bevy app to create for it.
    fn with_task_pool() {
        bevy::tasks::IoTaskPool::get_or_init(Default::default);
    }

    fn a_route() -> Route {
        Route {
            mode: TravelMode::Car,
            points: vec![[53.20, 6.55], [53.205, 6.55], [53.21, 6.55]],
            distance_m: 1000.0,
            duration_s: 60.0,
            steps: Vec::new(),
        }
    }

    /// A reroute must not blank the display. It keeps the previous route to draw
    /// and measure against, and — the part that does the actual work — it leaves
    /// the generation alone, which is what stops `update_route_visuals` despawning
    /// the line and rebuilding it from nothing.
    #[test]
    fn a_reroute_holds_the_previous_route_and_the_drawn_line() {
        with_task_pool();
        let mut route = RouteState::default();
        route.route = Some(a_route());
        let gen_before = route.generation;

        route.reroute_from(53.2001, 6.5502);

        assert!(route.route.is_none(), "the solved route is gone");
        assert!(route.held.is_some(), "…but the display still has one");
        assert!(route.displayed().is_some());
        assert_eq!(
            route.generation, gen_before,
            "an unchanged generation is what keeps the line on the map"
        );
        assert!(route.solved_for.is_none(), "and a solve is due");
        assert_eq!(route.to.coord, None, "only the start moved");
        assert_eq!(route.from.coord, Some((53.2001, 6.5502)));
    }

    /// Everything that is *not* a reroute must still take the line away — the whole
    /// reason `held` is set by one method rather than by `invalidate` itself.
    #[test]
    fn an_ordinary_edit_drops_a_held_route() {
        with_task_pool();
        let mut route = RouteState::default();
        route.route = Some(a_route());
        route.reroute_from(53.2001, 6.5502);
        assert!(route.held.is_some());

        // The user picks a new destination mid-reroute.
        route.set_endpoint_at(Endpoint::To, 53.30, 6.60);
        assert!(
            route.held.is_none(),
            "the held line must not outlive the trip"
        );
        assert!(route.displayed().is_none());
    }

    /// A reroute's corridor is a shorter, narrower version of the one already
    /// cached, so it must hit the cache — a full refetch is the seconds of blank
    /// display this exists to remove. And an overlapping corridor that is *not*
    /// contained must miss, or a solve would run on a graph missing its own roads.
    #[test]
    fn a_contained_corridor_reuses_the_cached_graph() {
        let wide = routing::Corridor::Direct(vec![(0, 0), (1, 0), (2, 0), (3, 0)]);
        let narrow = routing::Corridor::Direct(vec![(1, 0), (2, 0)]);
        assert!(covers(&wide, &narrow));
        assert!(covers(&wide, &wide), "equality is containment");
        assert!(!covers(&narrow, &wide));
        assert!(
            !covers(&wide, &routing::Corridor::Direct(vec![(2, 0), (4, 0)])),
            "one tile short is a miss, not a nearly-hit"
        );
        // Staged is three graphs on two lattices; containment is not asked there.
        assert!(!covers(
            &wide,
            &routing::Corridor::Staged {
                start: vec![(0, 0)],
                end: vec![(1, 0)],
                coarse: vec![(2, 0)],
            }
        ));
    }

    /// …but it must never stamp over a start the user chose. This is the one that
    /// would ruin a route: "Route from here" on the map, then opening the panel,
    /// and the picked start silently becoming the camera position again.
    #[test]
    fn defaulting_the_start_never_overwrites_one() {
        with_task_pool();
        let mut route = RouteState::default();
        route.set_endpoint_at(Endpoint::From, 53.10, 6.10);
        assert!(!route.default_start(Some((53.22, 6.57))));
        assert_eq!(route.from.coord, Some((53.10, 6.10)));

        // A half-typed label with no coordinate yet is still the user's.
        let mut route = RouteState::default();
        route.from.label = "Gron".into();
        assert!(!route.default_start(Some((53.22, 6.57))));
        assert_eq!(route.from.label, "Gron");
    }

    /// With nowhere to resolve "here" to (no anchor, no camera) it does nothing
    /// rather than inventing a start at null island.
    #[test]
    fn defaulting_the_start_needs_a_position() {
        let mut route = RouteState::default();
        assert!(!route.default_start(None));
        assert!(route.from.label.is_empty() && route.from.coord.is_none());
    }

    /// Setting an endpoint from the map — whether by "pick on map" or by the map's
    /// own "Route to here" — must drop the drawn route with it. Leaving the old
    /// line up under a new destination is the failure this shares between the two
    /// call sites, which is why they share the method.
    #[test]
    fn setting_an_endpoint_from_the_map_invalidates_the_route() {
        with_task_pool();
        let mut route = RouteState::default();
        route.route = Some(Route {
            mode: TravelMode::Car,
            points: vec![[53.20, 6.55], [53.21, 6.55]],
            distance_m: 1000.0,
            duration_s: 60.0,
            steps: Vec::new(),
        });
        route.picking = Some(Endpoint::To);
        let gen_before = route.generation;

        route.set_endpoint_at(Endpoint::To, 53.30, 6.60);

        assert!(route.route.is_none(), "the stale line must go");
        assert_ne!(route.generation, gen_before, "so the world visuals rebuild");
        assert!(route.picking.is_none(), "and the map-pick arming is spent");
        assert_eq!(route.to.coord, Some((53.30, 6.60)));
        assert!(route.solved_for.is_none(), "the solver must run again");
    }
}
