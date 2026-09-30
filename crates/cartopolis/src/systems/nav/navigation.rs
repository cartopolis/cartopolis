//! The route planner: the Bevy half of ADR-037.
//!
//! [`crate::systems::routing`] owns the graph and the search; this module owns
//! everything around them — fetching the corridor's tiles on a worker, drawing the
//! result in the world, and the planner.
//!
//! The planner is shaped like a phone map's, not like a desktop form: a **bottom
//! sheet over a map that stays visible** ([`render_route_sheet`]), with the travel
//! modes as one segmented row across the top, one summary line (time · distance),
//! the two endpoint fields and the turn list in a body that collapses, and the
//! button that starts the drive along the bottom. Each endpoint can be the current
//! position, a searched place, or a point picked straight off the map — the three
//! ways you actually name a destination. [`SheetDetent`] says why it is a sheet
//! and not the docked panel it used to be.
//!
//! **Why the solve is a thread and not a system.** A cold corridor is up to
//! [`routing::MAX_ROUTE_TILES`] HTTP fetches; even warm it is that many disk reads
//! plus a graph build. All of it is blocking, so it runs on a worker exactly like
//! the tile streamers do, and the sheet polls a `Mutex` for the answer. The built
//! graph is cached against its tile list, so flipping between Car/Bike/Walk re-runs
//! only the A* — which is what makes the mode chips feel instant.

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
use crate::systems::ground_stack::GroundLayer;
use crate::systems::icons;
use crate::systems::map_stream::MapStream;
use crate::systems::routing::{
    self, format_distance, format_duration, RoadGraph, Route, RouteError, TravelMode,
};
use crate::systems::theme;
use crate::systems::tile_loader::load_tile_image;
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
/// 0 (see `ground_stack::GroundLayer::y_offset`); buildings stand on 0. A route
/// drawn *below* the buildings' base would be swallowed wherever it passes an
/// arcade or a bridge pier, so it goes above.
///
/// Half a metre, not the twelve centimetres this first used: with `near = 0` and a
/// 200 km far plane the depth buffer cannot resolve a tenth of a metre at street
/// range, and the line came out shredded into horizontal bands where it lost the
/// fight to the ground beneath it. Physical separation alone still cannot settle
/// that — [`RouteMaterials`]' `depth_bias` is what actually does, exactly as
/// `ground_stack::GroundLayer::depth_bias` settles the surfaces among themselves —
/// but half a metre is a comfortable margin and still reads as painted on the road.
const ROUTE_Y: f32 = 0.5;

/// Half-widths in metres: a bright fill inside a darker casing, the way every road
/// on the basemap is drawn, so the route reads as part of the map rather than an
/// overlay floating on it.
/// A carriageway is 6–10 m, and the line has to stay readable from the few hundred
/// metres up that a route is actually looked at, where one metre is well under a
/// pixel — so it is drawn a shade wider than the street it follows, as every map
/// app draws it.
const ROUTE_HALF_WIDTH: f32 = 3.2;
const CASING_HALF_WIDTH: f32 = 4.8;

/// How wide the line has to be to stay visible, as a fraction of the camera's
/// altitude.
///
/// **A fixed metre width is invisible from any distance a whole route is looked
/// at.** Six metres of road is a comfortable fifteen pixels at a thousand metres
/// up and a *third* of a pixel at thirty kilometres — so planning a 30 km drive
/// and pressing Fit showed a bare map with two pins on it and no line between
/// them, with nothing wrong and nothing to say so. Every map app draws its route
/// in screen space for this reason; this line cannot, being world geometry that
/// lies in the road surface and takes the sun and the terrain with it, so the
/// width is scaled instead.
///
/// The number is one line's worth of screen: at altitude `a` a ~60° vertical
/// field of view covers about `1.15 a` metres over the viewport's height, so
/// `0.0017 a` is roughly 8 px of a 2772 px phone screen — about what the
/// reference apps draw, and it lands on the base width at ~1.9 km, below which
/// the real road width takes over again.
const ROUTE_WIDTH_PER_ALT: f32 = 0.0017;

/// Widest the line may get, as a multiple of the ground widths. Reached at ~120
/// km, well above the altitude the ground detail (and with it this line) is
/// hidden at — it is a backstop against a degenerate camera, not a working limit.
const ROUTE_WIDTH_MAX_SCALE: f32 = 64.0;

/// The width multiplier for a camera this high, **quantised to powers of two**.
///
/// Stepped rather than continuous for the same reason `street_labels`' size bands
/// are: the line is a baked mesh, and a width that tracked altitude exactly would
/// re-triangulate the whole route on every frame of a zoom — which is precisely
/// the per-frame churn that loses a mobile GPU. A doubling is a rebuild every few
/// seconds of a fast fly-out, and rounding (rather than flooring) keeps the drawn
/// width within √2 of the ideal in both directions.
#[must_use]
fn route_width_scale(altitude_m: f32) -> f32 {
    let wanted = altitude_m.max(0.0) * ROUTE_WIDTH_PER_ALT / ROUTE_HALF_WIDTH;
    if !wanted.is_finite() || wanted <= 1.0 {
        return 1.0;
    }
    wanted
        .log2()
        .round()
        .exp2()
        .clamp(1.0, ROUTE_WIDTH_MAX_SCALE)
}

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
/// claim, and bending them to the app's own palette would cost the convention.
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
    /// How much of the planner's sheet is up. See [`SheetDetent`].
    pub sheet: SheetDetent,
    /// Where the sheet landed last frame, in egui points; `None` when it is not
    /// up.
    ///
    /// Published for the reason [`crate::systems::dock::reserved_bottom`] is,
    /// and it is not decoration: the sheet is a floating `Area`, so it takes
    /// nothing out of the root `Ui` and `available_rect_before_wrap` cannot see
    /// it — while the place pins are floating `Area`s at the same `Order` drawn
    /// *later* in the frame, which is precisely what wins egui's hit test within
    /// an order (`place_pins`' module note). A pin left under the sheet is a
    /// teardrop nobody can see eating the taps meant for "Start".
    pub sheet_rect: Option<egui::Rect>,
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
            sheet: SheetDetent::default(),
            sheet_rect: None,
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

/// A `--ui route` run holds until the solve lands — otherwise the shot documents
/// an empty planner and a map with no line on it (ADR-037).
impl crate::systems::stream_watch::StreamBusy for RouteState {
    const NAME: &'static str = "routing";
    fn stream_busy(&self) -> bool {
        matches!(
            self.status,
            SolveStatus::Fetching(..) | SolveStatus::Solving
        )
    }
}

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
    mut nav: ResMut<GeoNavState>,
    tile_url: Res<OsmTileUrlTemplate>,
    camera_state: Res<crate::resources::CameraState>,
    harness: Option<Res<crate::systems::shot_harness::ShotHarness>>,
    time: Res<Time>,
    mut idle_s: Local<f32>,
    mut framed_for: Local<Option<((f64, f64), (f64, f64))>>,
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
                    // A reroute replaces a route already on screen and under a
                    // driver's nose; only a route the user just *asked* for gets
                    // framed. `held` is what tells the two apart, and it is
                    // cleared a line below.
                    let asked_for = route.held.is_none();
                    route.route = Some(r);
                    // The replacement is here, so the one the display was holding
                    // on to has done its job.
                    route.held = None;
                    route.status = SolveStatus::Idle;
                    // The answer is on the map now, so the sheet that asked the
                    // question gets out from in front of it. This is the whole
                    // reason the planner has detents at all — see [`SheetDetent`].
                    route.sheet = SheetDetent::Peek;
                    // …and the map is moved so the answer is actually *on* it.
                    // This replaces the "Fit" button, which asked the user to
                    // press a second thing to see the thing they had just asked
                    // for — every map app frames the route it has just solved.
                    //
                    // Not in Follow, and that is the one real cost of doing it
                    // automatically: a teleport brings the avatar with it (the
                    // camera is glued to the avatar there, so it cannot be framed
                    // any other way), and planning a route is not a request to be
                    // moved fifteen kilometres. The two map-looking modes get the
                    // framing; Follow keeps the view it had and drives from where
                    // it stands.
                    //
                    // A scripted capture is exempt: `--at` states where the shot
                    // is taken from, and a solve that moved the camera off it
                    // would quietly re-aim every `--ui route` preset and make the
                    // harness's "the metrics report the *actual* camera state"
                    // contract report a place nobody asked for.
                    //
                    // Once per pair of endpoints, and not once per solve:
                    // switching travel mode re-solves, and a camera the user has
                    // since panned being yanked back every time they tap Bike is
                    // the same over-eagerness the removed button avoided by being
                    // a button. "Autofit initially" is the ask.
                    let endpoints = route.from.coord.zip(route.to.coord);
                    let first_answer = endpoints.is_some() && *framed_for != endpoints;
                    if asked_for
                        && first_answer
                        && harness.is_none()
                        && !matches!(camera_state.mode, crate::resources::CameraMode::Avatar)
                    {
                        if let Some(tp) = frame_route(&route) {
                            nav.pending_teleport = Some(tp);
                            *framed_for = endpoints;
                        }
                    }
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
                    crate::platform::workers::submit(
                        "routing",
                        Box::new(move || {
                            let result = build_and_search(input, from, to, mode, &cache);
                            publish_solve(&sink, seq, result);
                        }),
                    );
                }
            }
        })
        .detach();
}

/// Where a detached solve leaves its answer: empty until it lands.
#[cfg(mcp)]
pub(crate) type DetachedSolve = Arc<Mutex<Option<Result<Route, RouteError>>>>;

/// Solve a route for something other than the Route panel: an agent walking the
/// avatar along streets rather than through buildings (`mcp::agent`).
///
/// The same fetch, graph build and search the panel runs, with a cache and a
/// progress counter of its own — so it neither replaces a route somebody has on
/// screen nor waits behind one. A raster basemap has no geometry to route on and
/// answers `NoData` at once.
#[cfg(mcp)]
pub(crate) fn solve_detached(
    from: (f64, f64),
    to: (f64, f64),
    mode: TravelMode,
    template: String,
) -> DetachedSolve {
    let sink: DetachedSolve = Arc::new(Mutex::new(None));
    if tile_source_kind(&template) != TileSourceKind::Vector {
        if let Ok(mut g) = sink.lock() {
            *g = Some(Err(RouteError::NoData));
        }
        return sink;
    }
    let out = sink.clone();
    IoTaskPool::get()
        .spawn(async move {
            let progress = (AtomicUsize::new(0), AtomicUsize::new(0));
            let cache: Arc<GraphCache> = Arc::new(Mutex::new(None));
            let publish = move |result| {
                if let Ok(mut g) = out.lock() {
                    *g = Some(result);
                }
            };
            match fetch_corridor(from, to, &template, &progress, &cache).await {
                Err(e) => publish(Err(e)),
                Ok(input) => crate::platform::workers::submit(
                    "agent route",
                    Box::new(move || publish(build_and_search(input, from, to, mode, &cache))),
                ),
            }
        })
        .detach();
    sink
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
    mut drawn: Local<Option<(u32, u32, u32, u32, u32)>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    decks: Res<crate::systems::map_geometry::DeckPlates>,
    globe: Res<GlobeState>,
    cameras: Query<(&GridCell, &Transform), With<FreeCamera>>,
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
    // The decks land some frames after the cell they belong to — their height is a
    // measurement — so a route planned over a viaduct is routinely drawn before the
    // viaduct exists. Its revision belongs in the redraw key for the same reason the
    // terrain's does.
    // How wide the line is drawn is part of what is baked into the mesh, so the
    // altitude band belongs in the redraw key beside the ground revisions — see
    // `route_width_scale` for why it is a band and not the altitude itself.
    let width_scale = cameras.single().map_or(1.0, |(cell, tf)| {
        route_width_scale(globe.altitude_m(cell, tf) as f32)
    });
    let key = (
        route.generation,
        stream.anchor_generation,
        terrain.revision,
        decks.revision,
        width_scale.to_bits(),
    );
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
            // Well clear of the ground stack. Matching the surfaces' own range was
            // not enough: the route has to win against the *buildings* and the road
            // surfaces too, and at street range the depth buffer's resolution is
            // coarse enough that a near-tie shows up as banding. The route is a
            // navigation aid — drawing it over the odd building corner is the right
            // trade against losing it under one.
            //
            // Which rung each of these is on is `ground_stack`'s to say, not this
            // file's. The four used to be hand-picked numbers here (18, 20, 22, 22)
            // on a ladder whose other end moved; see that module for what that cost.
            let p = RouteMaterials {
                fill: line(0.16, 0.53, 0.90, GroundLayer::RouteLine.depth_bias()),
                casing: line(0.06, 0.22, 0.42, GroundLayer::RouteCasing.depth_bias()),
                start: line(
                    ROUTE_START_RGB[0],
                    ROUTE_START_RGB[1],
                    ROUTE_START_RGB[2],
                    GroundLayer::RoutePin.depth_bias(),
                ),
                finish: line(
                    ROUTE_FINISH_RGB[0],
                    ROUTE_FINISH_RGB[1],
                    ROUTE_FINISH_RGB[2],
                    GroundLayer::RoutePin.depth_bias(),
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
    // The grades belong to the same slice of `points` the geometry was taken from.
    let road_levels = r.levels.get(1..n - 1).unwrap_or(&[]);
    let lift = route_lift(&road_world, road_levels, &decks);
    let (road, kept) = dedupe(road_world.iter().map(|w| to_local(*w)));
    if road.len() < 2 {
        return;
    }
    let road_lift: Vec<f32> = kept.iter().map(|&i| lift[i]).collect();
    let from_pin = to_local(to_world(&r.points[0]));
    let to_pin = to_local(to_world(&r.points[n - 1]));

    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    let ground = terrain.snapshot();
    let mut spawn = |mut mesh: Mesh, material: Handle<StandardMaterial>, at: DVec3| {
        // Follow the measured ground under the road. `ROUTE_Y` still lifts the line
        // clear of the surface stack; the terrain moves that whole stack, and a
        // route drawn on the old flat plane would run through the hill the street
        // goes over. Any bridge deck is already in the mesh (`route_lift`) and the
        // drape adds to it, so a viaduct ends up over the ground it stands on.
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
            // And, for the same reason, the same second component. An entity that
            // opts out of frustum culling is pushed into *every* cascade of every
            // light unconditionally, so a cross-town route was rasterised into up
            // to eight shadow maps to cast a shadow from a ribbon lying in the
            // road surface, half a metre above the ground it would be cast on.
            bevy::light::NotShadowCaster,
            RouteVisual,
            // Hidden from orbit with the rest of the ground detail (ADR-025).
            crate::components::HideInSpace,
        ));
    };

    let base = DVec3::new(origin.x, f64::from(ROUTE_Y), origin.z);
    spawn(
        ribbon(&road, &road_lift, CASING_HALF_WIDTH * width_scale),
        palette.casing.clone(),
        base,
    );
    spawn(
        ribbon(&road, &road_lift, ROUTE_HALF_WIDTH * width_scale),
        palette.fill.clone(),
        base,
    );

    // Pin-to-road connectors, drawn thin so a long one (the pin snapped across
    // water to reach a drivable road) reads as "get to the road", not as the route.
    // Skipped when the pin already sits on the road, the ordinary case.
    // The pin end stays on the ground — it is where the user aimed, not a place the
    // graph had an opinion about — so a connector to a road end on a deck ramps up to
    // it rather than hanging in the air.
    for (pin, road_end, end_lift, material) in [
        (from_pin, road[0], road_lift[0], palette.casing.clone()),
        (
            to_pin,
            *road.last().expect("road has ≥ 2 points"),
            *road_lift.last().expect("one lift per road point"),
            palette.casing.clone(),
        ),
    ] {
        if pin.distance(road_end) > CONNECTOR_MIN_M {
            spawn(
                ribbon(
                    &[pin, road_end],
                    &[0.0, end_lift],
                    CONNECTOR_HALF_WIDTH * width_scale,
                ),
                material,
                base,
            );
        }
    }

    // Start / finish discs, at the coordinates the user actually asked for.
    for (point, material) in [(from_pin, palette.start), (to_pin, palette.finish)] {
        spawn(
            disc(MARKER_RADIUS * width_scale),
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

/// Returns the surviving points **and the indices they came from**, so a caller with
/// a parallel array (the route's per-point grade) can thin it the same way. Two
/// arrays deduped independently would drift apart on the first collapsed pair, and
/// the symptom of that is a line that climbs a viaduct a few metres early.
fn dedupe(points: impl Iterator<Item = Vec2>) -> (Vec<Vec2>, Vec<usize>) {
    let mut out: Vec<Vec2> = Vec::new();
    let mut kept: Vec<usize> = Vec::new();
    for (i, p) in points.enumerate() {
        if out
            .last()
            .is_none_or(|last| last.distance_squared(p) > MIN_SEGMENT_M * MIN_SEGMENT_M)
        {
            out.push(p);
            kept.push(i);
        }
    }
    (out, kept)
}

/// How far above the ground plane each point of the drawn road path sits.
///
/// Zero everywhere the route runs on the ground, and the deck's height where it runs
/// over a bridge. The grade comes from the route (i.e. from the graph edge), never
/// from the position: a viaduct and the street it crosses occupy the same world XZ,
/// so a lift decided by `DeckPlates` alone would hoist an underpass onto the
/// structure above it.
///
/// A negative grade — a tunnel — is deliberately *not* pushed down. There is no
/// modelled tunnel bore to sink into, so the line would vanish under the map; the
/// route through a tunnel is drawn on the surface, as the basemap draws nothing
/// there at all.
fn route_lift(
    world: &[DVec3],
    levels: &[i8],
    decks: &crate::systems::map_geometry::DeckPlates,
) -> Vec<f32> {
    world
        .iter()
        .enumerate()
        .map(|(i, w)| {
            if levels.get(i).copied().unwrap_or(0) > 0 {
                decks.lift_at(Vec2::new(w.x as f32, w.z as f32))
            } else {
                0.0
            }
        })
        .collect()
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
///
/// **Height is per point**, and it is the only way a bridge can reach this mesh. The
/// deck a route crosses cannot be sampled from the ribbon's coordinates, because the
/// road on the viaduct and the road underneath it are at the same world XZ — only
/// the graph edge the point came from knows which, so the caller resolves it and
/// passes the answer in (see [`route_lift`]). `heights` is parallel to `points`.
fn ribbon(points: &[Vec2], heights: &[f32], half_width: f32) -> Mesh {
    debug_assert_eq!(points.len(), heights.len());
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Append a triangle wound so it faces +Y (up), or it is backface-culled and
    // invisible — the failure `ribbon_faces_up` guards. Each corner carries the
    // height of the centreline point it was offset from, so a step between two
    // points is a short ramp across the quad between them rather than a tear.
    let mut tri = |a: (Vec2, f32), b: (Vec2, f32), c: (Vec2, f32)| {
        // Y of (b−a)×(c−a): positive already faces up; otherwise swap b and c.
        let up = (b.0.x - a.0.x) * (c.0.y - a.0.y) - (b.0.y - a.0.y) * (c.0.x - a.0.x) < 0.0;
        let (b, c) = if up { (b, c) } else { (c, b) };
        let base = positions.len() as u32;
        for (p, y) in [a, b, c] {
            positions.push([p.x, y, p.y]);
        }
        indices.extend([base, base + 1, base + 2]);
    };

    let perp = |d: Vec2| Vec2::new(-d.y, d.x);
    let h = |i: usize| heights.get(i).copied().unwrap_or(0.0);

    // One butt-ended quad per segment.
    for i in 0..points.len().saturating_sub(1) {
        let (a, b) = (points[i], points[i + 1]);
        let (ya, yb) = (h(i), h(i + 1));
        let dir = (b - a).normalize_or_zero();
        if dir == Vec2::ZERO {
            continue;
        }
        let nrm = perp(dir) * half_width;
        tri((a + nrm, ya), (b + nrm, yb), (a - nrm, ya));
        tri((a - nrm, ya), (b + nrm, yb), (b - nrm, yb));
    }

    // Fill the wedge at each interior vertex (both sides — the inner one overlaps).
    for i in 1..points.len().saturating_sub(1) {
        let c = points[i];
        let y = h(i);
        let din = (points[i] - points[i - 1]).normalize_or_zero();
        let dout = (points[i + 1] - points[i]).normalize_or_zero();
        if din == Vec2::ZERO || dout == Vec2::ZERO {
            continue;
        }
        let (nin, nout) = (perp(din) * half_width, perp(dout) * half_width);
        tri((c, y), (c + nin, y), (c + nout, y));
        tri((c, y), (c - nin, y), (c - nout, y));
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

// ─── The planner sheet ───────────────────────────────────────────────────────

/// How much of the planner is up.
///
/// **The planner is a bottom sheet, not a docked panel, and the detents are the
/// reason.** A compact right panel *is* the viewport
/// (`egui_manager::responsive_right_panel`), so asking for a route to somewhere
/// covered the one thing you asked for: the map, with the route drawn on it.
/// The phone-map answer is the one taken here — the planner rises from the
/// bottom edge over a map that stays live, and the instant a route solves it
/// drops to [`Peek`](Self::Peek), which is the summary and the button that
/// starts the drive. `chat_overlay`'s module note records the same trade for
/// chat; this is the other panel that had no business owning the screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SheetDetent {
    /// The header alone: the travel modes, the summary line and the actions.
    /// What a solved route collapses to.
    Peek,
    /// Header plus body — the endpoints and the turn list. Where a planner with
    /// nothing to show yet always sits, because the fields *are* the content.
    #[default]
    Full,
}

/// Widest the sheet's *content* gets, in points — the frame's margins sit
/// outside it. A phone gets its viewport less the margins; past this a desktop
/// window would be spending a whole column on a two-field form, and a box
/// reaching both sides of a 2000 pt window stops reading as something sitting on
/// the map.
const SHEET_MAX_WIDTH: f32 = 420.0;
/// The Start button never gets narrower than this, whatever Clear has taken from
/// the row: it is the one control on the sheet you came to press.
const START_MIN_WIDTH_PTS: f32 = 96.0;
/// Most of the viewport height the sheet may claim. The rest is the point of the
/// exercise.
const SHEET_MAX_FRACTION: f32 = 0.55;
/// What the always-up parts cost, in points — grabber, mode chips, summary,
/// action row. An estimate, and it only has to be close: it is subtracted from
/// the height budget to size the *scrolling* body, so being a few points out
/// moves where that body starts scrolling, never the sheet's edge.
const SHEET_HEADER_PTS: f32 = 150.0;
/// The body never shrinks below this, even on a viewport too short to honour
/// [`SHEET_MAX_FRACTION`]. Same rule as [`waypoint_field_width`]'s floor:
/// overflow visibly rather than collapse a control to nothing.
const SHEET_BODY_MIN_PTS: f32 = 96.0;

/// Height of the grabber's row, the bar drawn in it, and how thick that bar is.
const GRABBER_ROW_PTS: f32 = 16.0;
const GRABBER_WIDTH_PTS: f32 = 40.0;
const GRABBER_THICKNESS_PTS: f32 = 4.0;
/// Points of drag **in one frame** that count as a flick on the grabber.
///
/// Read per frame rather than accumulated from a drag origin on purpose: the
/// answer is a two-state detent, so the first frame that clears the threshold
/// has already said everything a longer drag could, and the alternative is
/// cross-frame state for a control whose entire range is one bit.
const GRABBER_FLICK_PTS: f32 = 3.0;

/// How tall the scrolling body may be, given the viewport and how far up from
/// the bottom edge the sheet is sitting.
#[must_use]
fn sheet_body_height(viewport_h: f32, bottom: f32) -> f32 {
    (viewport_h * SHEET_MAX_FRACTION - bottom - SHEET_HEADER_PTS).max(SHEET_BODY_MIN_PTS)
}

/// Which detent the sheet is actually drawn at, which is not always the one the
/// user last asked for.
///
/// Two overrides, both of them "the sheet is in the way of the next thing you
/// have to do". With no route there is nothing to collapse *to* — the fields are
/// the content — and while an endpoint is armed for a map click the map is what
/// needs to be reachable, so the sheet drops however it was left.
#[must_use]
fn effective_detent(asked: SheetDetent, has_route: bool, picking: bool) -> SheetDetent {
    if picking {
        return SheetDetent::Peek;
    }
    if !has_route {
        return SheetDetent::Full;
    }
    asked
}

/// Where the sheet's bottom edge sits, as an offset up from the bottom of the
/// viewport: clear of the dock, or clear of the on-screen keyboard when that is
/// up instead.
///
/// Keyed off the keyboard being *open* rather than off `text_edit_focused()`
/// because [`OnScreenKeyboard`](crate::systems::onscreen_keyboard::OnScreenKeyboard)
/// holds its target across the frame egui takes focus away, so the lift does not
/// flicker every time a tap lands between two fields. It costs one frame on the
/// way in — the keys' own layout, which runs after this system, sees the field
/// still down at the bottom and puts that first slab on the top edge — and
/// self-corrects on the next frame, when the sheet has moved up and the field is
/// clear. `chat_overlay`'s composer cannot afford even that (it never lifts, so
/// the flip would stick) and predicts instead; a sheet that does lift can just
/// take the frame.
fn sheet_bottom(
    egui: &EguiManager,
    keyboard: &crate::systems::onscreen_keyboard::OnScreenKeyboard,
) -> f32 {
    if keyboard.is_open() {
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui.viewport_pts());
        return crate::systems::onscreen_keyboard::expected_height(
            viewport,
            egui.ui_scale(),
            egui.display_scale(),
        ) + theme::OSD_GAP;
    }
    crate::systems::dock::reserved_bottom() + theme::OSD_MARGIN
}

/// The grabber: the short bar that says the sheet moves, and the control that
/// moves it. Returns the detent it was asked to go to, if any.
fn grabber(ui: &mut egui::Ui, detent: SheetDetent) -> Option<SheetDetent> {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), GRABBER_ROW_PTS),
        egui::Sense::click_and_drag(),
    );
    let live = resp.hovered() || resp.dragged();
    ui.painter().rect_filled(
        egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(GRABBER_WIDTH_PTS, GRABBER_THICKNESS_PTS),
        ),
        egui::CornerRadius::same(theme::RADIUS_PILL),
        if live { theme::ACCENT } else { theme::LINE },
    );
    if resp.clicked() {
        return Some(match detent {
            SheetDetent::Peek => SheetDetent::Full,
            SheetDetent::Full => SheetDetent::Peek,
        });
    }
    let dy = resp.drag_delta().y;
    if dy <= -GRABBER_FLICK_PTS {
        Some(SheetDetent::Full)
    } else if dy >= GRABBER_FLICK_PTS {
        Some(SheetDetent::Peek)
    } else {
        None
    }
}

/// The travel modes, as one segmented row of equal-width chips.
///
/// Equal width and not size-to-content: three chips whose widths follow their
/// words ("Car", "Bike", "Walk") read as three unrelated buttons, and this is
/// one control with three positions. Icon *and* word — the picture is what is
/// read at a glance, over and over, and the words are short enough to keep
/// beside it.
fn mode_chips(ui: &mut egui::Ui, current: TravelMode, reserved: f32, actions: &mut PanelActions) {
    let n = TravelMode::ALL.len() as f32;
    let gap = ui.spacing().item_spacing.x;
    let width = ((ui.available_width() - reserved - (n - 1.0) * gap) / n).max(36.0);
    let size = egui::vec2(width, theme::row_height());
    for mode in TravelMode::ALL {
        let selected = current == mode;
        let label = format!("{}  {}", icons::travel_mode(mode), mode.label());
        if theme::chip_sized(ui, &label, selected, size).clicked() && !selected {
            actions.mode = Some(mode);
        }
    }
}

/// The route planner: a bottom sheet, over a map that stays visible.
#[allow(clippy::too_many_arguments)]
pub fn render_route_sheet(
    egui: Res<EguiManager>,
    mut route: ResMut<RouteState>,
    mut right_panel: ResMut<RightPanel>,
    mut nav: ResMut<GeoNavState>,
    mut driver: ResMut<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    map_anchor: Res<MapAnchor>,
) {
    if !right_panel.is(RightPanel::Route) {
        // Read before writing: an unconditional store would mark `RouteState`
        // changed on every frame of a session that never opens the planner.
        if route.sheet_rect.is_some() {
            route.sheet_rect = None;
        }
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

    let viewport = egui.viewport_pts();
    let bottom = sheet_bottom(&egui, &keyboard);
    let body_height = sheet_body_height(viewport.y, bottom);
    // The width the *content* gets. The frame's own margins are outside it, so
    // they come off first — sized against the raw viewport the sheet reaches both
    // screen edges on a phone and reads as a slab rather than as a card lying on
    // the map.
    let frame = theme::overlay_frame();
    let inset = frame.inner_margin.sum().x + 2.0 * frame.stroke.width;
    let width = (viewport.x - 2.0 * theme::OSD_MARGIN - inset).min(SHEET_MAX_WIDTH);
    let detent = effective_detent(route.sheet, route.route.is_some(), route.picking.is_some());

    let mut actions = PanelActions::default();
    let mut close = false;
    let mut asked_detent = None;
    let ctx = egui.ctx();
    let area = egui::Area::new(egui::Id::new("route_sheet"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(ctx, |ui| {
            frame.show(ui, |ui| {
                // Pin the width before anything is laid out: this is a floating
                // `Area`, so a child reaching for the available width would grow
                // the sheet a little every frame (the trap `hud`'s module note
                // documents at length), and the scrolling body below has no width
                // at all to scroll inside until something states one.
                ui.set_width(width);

                asked_detent = grabber(ui, route.sheet);

                // Travel modes across the top, with the ✕ in the corner. Organic
                // Maps puts the modes above the fields because the mode changes
                // what a sensible destination even is.
                ui.horizontal(|ui| {
                    let reserved = theme::close_button_size() + ui.spacing().item_spacing.x;
                    mode_chips(ui, route.mode, reserved, &mut actions);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = theme::close_button(ui).clicked();
                    });
                });
                ui.add_space(6.0);

                // The one line the planner exists to produce, or what is standing
                // between us and it.
                summary_line(ui, &route);

                // The map-pick prompt lives *outside* the body, because arming a
                // pick is what collapses the sheet: the instruction has to survive
                // the collapse it causes.
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

                if detent == SheetDetent::Full {
                    ui.add_space(6.0);
                    // One scroll for the whole body — the endpoints *and* the turn
                    // list. Two would let the fields scroll away from under the
                    // finger typing into them, and the sheet's height budget is a
                    // single number to spend.
                    egui::ScrollArea::vertical()
                        .max_height(body_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            // The endpoint card. **The destination is the primary
                            // field and the start is a footnote**, because "route to
                            // somewhere" is the whole act and the start is almost
                            // always "here" — which the app already knows. The first
                            // cut gave both ends an identical full-width row, which
                            // made every route a two-field form where one field had
                            // an obvious answer, and left a blank start looking like
                            // a required step.
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                start_row(ui, &mut route, here, &mut actions);
                                endpoint_row(ui, &mut route, Endpoint::To, here, &mut actions);
                            });
                            if route.route.is_some() {
                                ui.add_space(6.0);
                                ui.separator();
                                turn_list(ui, &route, &mut actions);
                            }
                        });
                }

                if route.route.is_some() {
                    ui.add_space(8.0);
                    action_row(ui, &mut actions);
                }
            });
        });

    route.sheet_rect = Some(area.response.rect);
    if let Some(d) = asked_detent {
        route.sheet = d;
    }
    let drive = actions.drive;
    apply_actions(actions, &mut route, &mut nav, here);
    // Driving takes the whole screen over, so the planner that started it gets out
    // of the way — the turn list is on the manoeuvre card now.
    if drive && route.route.is_some() {
        driver.enter();
        *right_panel = RightPanel::None;
    }
    if close {
        *right_panel = RightPanel::None;
    }
}

/// The summary: time · distance when there is a route, and what is in the way of
/// one when there is not.
fn summary_line(ui: &mut egui::Ui, route: &RouteState) {
    match (&route.status, &route.route) {
        (SolveStatus::Fetching(done, total), _) => {
            if *total > 0 {
                theme::busy_progress(ui, "Reading map data…", *done, *total);
            } else {
                theme::busy(ui, "Reading map data…");
            }
        }
        (SolveStatus::Solving, _) => theme::busy(ui, "Finding the best route…"),
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
        }
        (SolveStatus::Idle, None) => {
            ui.label(
                egui::RichText::new("Set a start and a destination to plan a route.")
                    .size(theme::SIZE_SUB)
                    .color(theme::TEXT_DIM),
            );
        }
    }
}

/// What a solved route affords, along the bottom of the sheet.
///
/// Driving is what a solved route is *for*, so it takes the primary slot and the
/// right-hand end, where a thumb is.
///
/// There was a "Fit" button here. It framed the route on the map — which is what
/// the user wanted the instant the route appeared, not one tap later, so
/// `drive_route_solver` now does it on the solve and the button is gone. A
/// control whose only job is to finish an action the app could have finished
/// itself is a control that should not be on a phone's bottom sheet.
fn action_row(ui: &mut egui::Ui, actions: &mut PanelActions) {
    ui.horizontal(|ui| {
        if ui.button("Clear").clicked() {
            actions.clear = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let width = ui.available_width().max(START_MIN_WIDTH_PTS);
            if theme::primary_button_sized(ui, &format!("{}  Start", icons::PLAY), width)
                .on_hover_text(
                    "Follow this route: strip the map, lock the camera \
                     behind the arrow, and show the next turn",
                )
                .clicked()
            {
                actions.drive = true;
            }
        });
    });
}

/// The turn list. Each row flies the camera to its manoeuvre.
fn turn_list(ui: &mut egui::Ui, route: &RouteState, actions: &mut PanelActions) {
    let Some(r) = &route.route else {
        return;
    };
    for (i, step) in r.steps.iter().enumerate() {
        let resp = ui
            .horizontal(|ui| {
                ui.label(
                    egui::RichText::new(icons::maneuver(step.maneuver))
                        .size(theme::SIZE_VALUE)
                        .color(theme::ACCENT_TEXT),
                );
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(step.instruction()).size(theme::SIZE_SUB));
                    if step.distance_m >= 1.0 {
                        ui.label(
                            egui::RichText::new(format_distance(step.distance_m))
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
}

/// What the panel's widgets asked for this frame. Collected rather than applied
/// inline because the widgets borrow `route` while they draw.
#[derive(Default)]
struct PanelActions {
    swap: bool,
    clear: bool,
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
    // **Which way it opens depends on where the field is**, and since the planner
    // became a bottom sheet the field is near the bottom edge — a dropdown that
    // always drops *down* would open off the screen, i.e. the suggestions for the
    // one control this whole panel is about would be invisible. Pivoting on the
    // field's top edge instead of its bottom is the whole fix, and it needs no
    // height: egui grows the box away from the pivot.
    let drops_down = field_rect.center().y < screen.center().y;
    let (pivot, mut popup_pos) = if drops_down {
        (
            egui::Align2::LEFT_TOP,
            field_rect.left_bottom() + egui::vec2(0.0, 2.0),
        )
    } else {
        (
            egui::Align2::LEFT_BOTTOM,
            field_rect.left_top() - egui::vec2(0.0, 2.0),
        )
    };
    popup_pos.x = popup_pos
        .x
        .min(screen.right() - popup_width - 16.0)
        .max(screen.left() + 16.0);
    egui::Area::new(egui::Id::new(("route_suggestions", hint)))
        .order(egui::Order::Foreground)
        .pivot(pivot)
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
    if let Some(i) = actions.goto_step {
        if let Some(step) = route.route.as_ref().and_then(|r| r.steps.get(i)) {
            nav.pending_teleport = Some(Teleport::geo(step.at[0], step.at[1], Some(220.0)));
        }
    }
}

/// Highest the framing will pull back to, in metres.
///
/// **Deliberately under [`globe::GLOBE_SHOW_ALT`].** The drawn route carries
/// `HideInSpace` along with the rest of the ground detail, so a framing that
/// crossed the space tier would fly to a viewpoint from which the thing being
/// framed is not drawn at all — the previous 60 km ceiling did exactly that for
/// any route over about 65 km. Past this the framing shows the middle of a long
/// route instead of the whole of it, which is the right way round: some of the
/// line beats none of it.
const FRAME_MAX_ALT: f64 = crate::systems::globe::GLOBE_SHOW_ALT as f64 * 0.9;

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
    Some(
        Teleport::geo(
            mid_lat,
            (min_lng + max_lng) / 2.0,
            // A ~60° vertical field of view frames a span from roughly its own
            // length away; the margin keeps the endpoints off the screen edge.
            Some((span_m * 0.9).clamp(200.0, FRAME_MAX_ALT)),
        )
        // Silent (`Teleport::silent`): this is the camera framing what was just
        // solved, not somebody choosing to go somewhere — which is the same
        // reason the caller skips it in Follow. It also only ever runs in the two
        // modes where there is no avatar on screen to watch arrive.
        .silently(),
    )
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

    /// The whole point of the sheet: whatever the screen, most of it is still map.
    ///
    /// The floor is the deliberate exception — on a viewport too short to honour
    /// the fraction the body overflows visibly rather than collapsing to nothing,
    /// the same trade [`waypoint_field_width`] makes.
    #[test]
    fn the_sheet_leaves_the_map_visible() {
        // A phone in portrait, a phone in landscape, a laptop, a big monitor.
        for viewport_h in [640.0_f32, 320.0, 800.0, 1400.0] {
            let bottom = 60.0;
            let body = sheet_body_height(viewport_h, bottom);
            let total = bottom + SHEET_HEADER_PTS + body;
            assert!(
                total <= viewport_h * SHEET_MAX_FRACTION + 0.01 || body == SHEET_BODY_MIN_PTS,
                "a {viewport_h} pt viewport gave the sheet {total} pt"
            );
            assert!(
                body >= SHEET_BODY_MIN_PTS,
                "the body collapsed at {viewport_h}"
            );
        }
    }

    /// The two cases where the sheet is not where the user last left it, both of
    /// them "it is standing in front of the next thing you have to do".
    #[test]
    fn the_sheet_gets_out_of_the_way_by_itself() {
        // Nothing to collapse to: the fields are the content.
        assert_eq!(
            effective_detent(SheetDetent::Peek, false, false),
            SheetDetent::Full
        );
        // A map pick needs the map, so it wins even over a planner with no route.
        assert_eq!(
            effective_detent(SheetDetent::Full, false, true),
            SheetDetent::Peek
        );
        assert_eq!(
            effective_detent(SheetDetent::Full, true, true),
            SheetDetent::Peek
        );
        // Otherwise the user's own answer stands, both ways round.
        assert_eq!(
            effective_detent(SheetDetent::Full, true, false),
            SheetDetent::Full
        );
        assert_eq!(
            effective_detent(SheetDetent::Peek, true, false),
            SheetDetent::Peek
        );
    }

    /// The ribbon must be a proper triangle mesh whose width matches the request —
    /// the route is unusable if the line is inverted, degenerate, or hairline.
    #[test]
    fn ribbon_has_the_requested_width() {
        let points = vec![Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0)];
        let mesh = ribbon(&points, &vec![0.0; points.len()], 3.0);
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
        let mesh = ribbon(&points, &vec![0.0; points.len()], 3.0);
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
        let mesh = ribbon(&points, &vec![0.0; points.len()], HW);
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
    #[ignore = "fetches real tiles over the network"]
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
    #[ignore = "printing hatch: it reports numbers, it does not assert them"]
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
    #[ignore = "printing hatch: it reports numbers, it does not assert them"]
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
                    if let Ok(td) = crate::systems::vector_tiles::TileData::parse(&body) {
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
        let (pts, kept) = pts;
        assert_eq!(pts.len(), 2, "got {pts:?}");
        assert_eq!(pts[0], Vec2::new(0.0, 0.0));
        assert_eq!(pts[1], Vec2::new(50.0, 0.0));
        // The surviving indices are what keeps a parallel array (the per-point
        // grade) in step with the thinned geometry.
        assert_eq!(
            kept,
            vec![0, 3],
            "the kept indices must name the input points"
        );
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

    /// A route has to stay visible from the altitude that frames it, and at
    /// street level it still has to look like a road marking rather than a
    /// motorway.
    ///
    /// The regression this pins: the drawn line is world geometry a fixed 6.4 m
    /// wide, which is a third of a pixel from the 30 km a cross-province route is
    /// framed at — the route was *there*, correct, and invisible.
    #[test]
    fn the_route_line_stays_wide_enough_to_see() {
        // Ground level and a city view keep the real road width.
        assert!((route_width_scale(0.0) - 1.0).abs() < f32::EPSILON);
        assert!((route_width_scale(500.0) - 1.0).abs() < f32::EPSILON);

        // Every altitude a route is looked at from lands within a factor of √2 of
        // one line's worth of screen — which is what the power-of-two bands buy in
        // exchange for not re-triangulating the mesh on every frame of a zoom.
        for altitude in [2_000.0_f32, 5_000.0, 16_000.0, 30_000.0, 45_000.0] {
            let drawn = ROUTE_HALF_WIDTH * route_width_scale(altitude);
            let ideal = altitude * ROUTE_WIDTH_PER_ALT;
            let ratio = drawn / ideal;
            assert!(
                (0.70..=1.42).contains(&ratio),
                "at {altitude} m the line is {drawn} m against an ideal {ideal} m",
            );
        }

        // Monotone, so a climb never *narrows* the line, and bounded.
        let mut previous = 0.0;
        for step in 0..200 {
            let scale = route_width_scale(step as f32 * 1_000.0);
            assert!(scale >= previous, "the line narrowed at {step} km");
            assert!(scale <= ROUTE_WIDTH_MAX_SCALE);
            previous = scale;
        }
        // Nothing pathological from a camera that has gone wrong.
        assert!((route_width_scale(f32::NAN) - 1.0).abs() < f32::EPSILON);
        assert!(route_width_scale(f32::INFINITY) <= ROUTE_WIDTH_MAX_SCALE);
    }

    /// The framing altitude has to scale with the route, and stay inside the band
    /// where the map tier still streams detail.
    #[test]
    fn framing_scales_with_the_route() {
        let mut route = RouteState::default();
        let mk = |span: f64| Route {
            mode: TravelMode::Car,
            points: vec![[53.20, 6.55], [53.20 + span, 6.55]],
            levels: vec![0; 2],
            distance_m: span * 111_320.0,
            duration_s: 60.0,
            steps: Vec::new(),
        };

        route.route = Some(mk(0.001));
        let short = frame_route(&route).unwrap().altitude.unwrap();
        route.route = Some(mk(0.1));
        let long = frame_route(&route).unwrap().altitude.unwrap();

        assert!(long > short, "a longer route must be framed from higher up");
        assert!((200.0..=FRAME_MAX_ALT).contains(&short));
        assert!((200.0..=FRAME_MAX_ALT).contains(&long));

        // A route far longer than the tier can frame is still framed *inside* the
        // tier — above `GLOBE_SHOW_ALT` the line is not drawn, so pulling further
        // back would frame an empty map. `MAX_STAGED_KM` is 360.
        route.route = Some(mk(3.5));
        let cross_country = frame_route(&route).unwrap().altitude.unwrap();
        assert!(
            cross_country < f64::from(crate::systems::globe::GLOBE_SHOW_ALT),
            "framed at {cross_country} m, where the route is hidden",
        );
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
            levels: vec![0; 3],
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
            levels: vec![0; 2],
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
