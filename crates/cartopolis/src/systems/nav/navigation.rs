//! The route planner: the Bevy half of ADR-037.
//!
//! [`crate::systems::routing`] owns the graph and the search; this module owns
//! everything around them — fetching the corridor's tiles on a worker, drawing the
//! result in the world, and the planner.
//!
//! The planner is shaped like a phone map's, not like a desktop form: a **bottom
//! sheet over a map that stays visible** (`lib_route`, fed by [`drive_route_sheet`]), with the travel
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
use crate::grid::{anchored_position_f64, AnchoredCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::platform::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use web_time::Instant;

use bevy::asset::RenderAssetUsages;
use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::grid::{lat_lng_to_world, world_to_lat_lng};
use crate::platform::http;
use crate::platform::location::LocationWatch;
use crate::resources::{MapAnchor, OsmTileUrlTemplate, RightPanel};
use crate::systems::geo_nav::{
    camera_geo, nominatim_base_url, percent_encode, GeoNavState, GeoSuggestion, Teleport,
};
use crate::systems::globe::GlobeState;
use crate::systems::ground_stack::GroundLayer;
use crate::systems::icons;
use crate::systems::map_stream::MapStream;
use crate::systems::routing::{self, format_distance, RoadGraph, Route, RouteError, TravelMode};
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
/// The number is one line's worth of screen. It was `0.0017 a`, "8 px of a
/// 2772 px phone screen" — but that phone is ~920 *logical* points tall, so the
/// line came out under 3 pt there and ~3 px on an 800 px desktop window
/// (measured 2026-11-02, Groningen from 4.5 km): a pale thread, where the
/// reference apps draw 5–8 pt. At `0.0034 a` and Bevy's 45° vertical field of
/// view (`0.83 a` metres over the viewport's height) the line is about six
/// points on either, and it lands on the base width at ~0.9 km, below which the
/// real road width takes over again — so the chase rig's view is unchanged.
const ROUTE_WIDTH_PER_ALT: f32 = 0.0034;

/// Widest the line may get, as a multiple of the ground widths. Reached at ~60
/// km, above the altitude the ground detail (and with it this line) is hidden at
/// — it is a backstop against a degenerate camera, not a working limit.
const ROUTE_WIDTH_MAX_SCALE: f32 = 64.0;

/// Below this camera height the line is a **footpath**, not a road on a map.
///
/// The ground widths are drawn a shade wider than a carriageway so a route reads
/// from a few hundred metres up; from behind a walking avatar the same 6.4 m band
/// and 14 m start disc covered a whole square and the avatar standing in it
/// (`walk_guide`, 2026-10-06). Only the street-level camera is ever this low —
/// driver mode's chase rig sits at ~65 m — so this is the walking view's width
/// without asking which mode is on.
const STREET_LEVEL_ALT: f32 = 40.0;

/// The width multiplier at street level: a 0.8 m path in a 1.2 m casing, and
/// 2 m markers in a 2.8 m ring — a footpath beside an avatar about half a metre across. 0.3
/// was tried first and still read as a road laid across the square.
const STREET_LEVEL_SCALE: f32 = 0.125;

/// How high the line floats at street level, in metres.
///
/// [`ROUTE_Y`]'s half metre is a margin for the far field; from behind an avatar
/// it is knee height, and the line reads as a plank hovering over the pavement.
/// Up close the depth buffer has precision to spare, and the depth bias that
/// settles the line against the ground at range still applies.
const STREET_LEVEL_Y: f32 = 0.15;

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
    if altitude_m.is_finite() && altitude_m < STREET_LEVEL_ALT {
        return STREET_LEVEL_SCALE;
    }
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

/// Radius of the start/finish discs, and of the white ring round each as a
/// multiple of it. The ring's 11.2 m is a little over twice the casing's
/// half-width, which is what makes a pin read as a mark *on* the line rather than
/// a bulge in it.
const MARKER_RADIUS: f32 = 8.0;
const MARKER_RIM_SCALE: f32 = 1.4;

/// How far the pins rise over the ground per width band above the base, in
/// metres: 24 m from the 2× band (~1.3 km up), clear of Groningen's ordinary roofs.
/// See `update_route_visuals`.
const PIN_RISE_M: f32 = 12.0;

/// How far each layer of the drawn route — casing, fill, pin ring, pin — floats
/// over the one below, in metres at the base width (scaled with it). See
/// `update_route_visuals` for why the depth rungs alone were not enough.
const LAYER_LIFT_M: f32 = 0.1;

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

    /// The places the field's lookup answered, while they are up.
    #[must_use]
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub fn suggestions(&self) -> &[GeoSuggestion] {
        if self.show_suggestions {
            &self.suggestions
        } else {
            &[]
        }
    }

    /// The field's lookup is under way.
    #[must_use]
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub fn looking_up(&self) -> bool {
        self.in_flight
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
    /// The same endpoints routed for the other travel modes, over the same graph
    /// ([`other_modes`]). Empty when the solve failed or the corridor was staged.
    others: Vec<Route>,
}

/// The route planner's state.
#[derive(Resource)]
pub struct RouteState {
    pub from: Waypoint,
    pub to: Waypoint,
    pub mode: TravelMode,
    pub route: Option<Route>,
    /// Every mode's route between the current endpoints, the selected one
    /// included, as far as they were solved ([`other_modes`]).
    ///
    /// This is what puts a time on every mode chip, and what makes switching
    /// mode free: the corridor and the graph are the same for all three, so one
    /// fetch answers all of them and a tap on Bike swaps a route in rather than
    /// starting a solve. Cleared with the route by [`Self::invalidate`].
    pub by_mode: Vec<Route>,
    /// The destination is a person sharing their location in a private chat, as
    /// `(chat key, username)`, and moves with them ([`follow_shared_location`]).
    ///
    /// Dropped by anything else that sets the destination, so a route to Sanne
    /// that is then pointed at a café stops chasing Sanne.
    pub following: Option<(String, String)>,
    /// The door a walking destination was moved to ([`snap_walk_to_door`]).
    /// Compared against the destination rather than cleared by every setter, so
    /// "to the door" is only ever said about the door the route actually ends at.
    pub door: Option<(f64, f64)>,
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
    /// The destination is the primary field, and the start
    /// only becomes one while it is being changed.
    pub from_editing: bool,
    /// How much of the planner's sheet is up. See [`SheetDetent`].
    pub sheet: SheetDetent,
    /// Where the sheet is, in UI points (y down, `min` the top-left corner);
    /// `None` when it is not up.
    ///
    /// Published for the reason [`crate::systems::dock::reserved_bottom`] is:
    /// the sheet floats over the map, so nothing else knows it covers it, and
    /// the place pins have to stay out from under it. A pin left under the
    /// sheet is a teardrop nobody can see eating the taps meant for "Start".
    pub sheet_rect: Option<Rect>,
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
    /// What [`drive_route_sheet`] hands the library's sheet (`lib_route`)
    /// each frame: what needs the avatar, the camera and the keyboard.
    pub lib_view: RouteView,
    /// A trip the library's sheet asked to start, started by
    /// [`drive_route_sheet`], which holds the driver, the walk guide and the
    /// camera.
    pub lib_trip: Option<Trip>,
}

/// The side of the planner the library's sheet (`lib_route`) cannot see
/// itself: published each frame by [`drive_route_sheet`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RouteView {
    /// Where "My position" is: the avatar, or the camera with no avatar.
    pub here: Option<(f64, f64)>,
    /// This device gives a position fix (riding and driving follow it).
    pub gps: bool,
    /// There is an avatar to walk the route.
    pub avatar: bool,
    /// The sheet's bottom edge above the viewport's, in points: clear of the
    /// dock or of the on-screen keyboard ([`sheet_bottom`]).
    pub bottom: f32,
    /// The viewport, in points.
    pub viewport: [f32; 2],
    /// The solved route's arrival on the clock, "14:32".
    pub arrive: Option<String>,
}

/// Starting along the solved route, as the sheet's buttons ask for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trip {
    /// Ride or drive it (`systems::driver`).
    Drive,
    /// Walk it in the world (`systems::walk_guide`); `auto` is "Walk me
    /// there", the avatar following the route by itself.
    Walk { auto: bool },
}

/// The library's sheet's outer width for a viewport this wide, in points:
/// [`SHEET_MAX_WIDTH`] of content plus a frame's padding.
#[must_use]
pub fn lib_sheet_width(viewport_w: f32) -> f32 {
    (viewport_w - 2.0 * theme::OSD_MARGIN)
        .min(SHEET_MAX_WIDTH + 40.0)
        .max(0.0)
}

/// The library's header: the height its `lib_route` header is drawn at.
pub const LIB_SHEET_HEADER_PTS: f32 = 56.0;

/// What the library's sheet at its peek is taken to cost, for the pins
/// ([`RouteState::sheet_rect`]); it is laid out by `bevy_ui` after this
/// is published.
const LIB_SHEET_PEEK_PTS: f32 = 250.0;

/// The tallest the library's sheet gets: [`SHEET_MAX_FRACTION`] of the
/// viewport plus its header, and never past the top of the screen.
#[must_use]
pub fn lib_sheet_max_height(viewport_h: f32, bottom: f32) -> f32 {
    (viewport_h * SHEET_MAX_FRACTION + LIB_SHEET_HEADER_PTS)
        .min(viewport_h - bottom - theme::OSD_MARGIN)
        .max(SHEET_BODY_MIN_PTS)
}

impl Default for RouteState {
    fn default() -> Self {
        Self {
            from: Waypoint::default(),
            to: Waypoint::default(),
            // Walking first: it is what the app does in the world, and riding
            // or driving needs a location fix most devices here do not have
            // (`docs/mockups/ux-review`, `routing-redesign-direction`).
            mode: TravelMode::Foot,
            route: None,
            by_mode: Vec::new(),
            following: None,
            door: None,
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
            lib_view: RouteView::default(),
            lib_trip: None,
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
        self.by_mode.clear();
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
    ///
    /// Named rather than picked ([`Self::set_endpoint_named`]): the start is where
    /// the vehicle is, which already has a name. Through `set_endpoint_at` every
    /// reroute relabelled it as raw coordinates and spent a reverse-geocode round
    /// trip to arrive at a street address nobody asked for.
    pub fn reroute_from(&mut self, lat: f64, lng: f64) {
        let previous = self.route.take();
        self.set_endpoint_named(Endpoint::From, HERE_LABEL, lat, lng);
        self.held = previous;
    }

    /// Switch travel mode: swap in the route already solved for it, or ask for a
    /// solve when there is none.
    ///
    /// The swap keeps `solved_for` pointing at the new mode, so the solver sees
    /// nothing to do; the generation bump is what redraws the line.
    pub fn select_mode(&mut self, mode: TravelMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        let ready = self.by_mode.iter().find(|r| r.mode == mode).cloned();
        match (ready, self.solved_for) {
            (Some(r), Some((from, to, _))) => {
                self.route = Some(r);
                self.held = None;
                self.solved_for = Some((from, to, mode));
                self.status = SolveStatus::Idle;
                self.generation = self.generation.wrapping_add(1);
            }
            _ => {
                // Only the search re-runs — `solve` reuses the cached graph.
                self.solved_for = None;
                self.status = SolveStatus::Idle;
            }
        }
    }

    /// The solved duration for `mode` between the current endpoints, if any.
    #[must_use]
    pub fn duration_for(&self, mode: TravelMode) -> Option<f64> {
        self.by_mode
            .iter()
            .find(|r| r.mode == mode)
            .map(|r| r.duration_s)
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
        if end == Endpoint::To {
            self.following = None;
        }
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
        if end == Endpoint::To {
            self.following = None;
        }
        self.waypoint_mut(end).set(label.into(), (lat, lng));
        self.invalidate();
    }

    /// Move the destination to where the person it follows is now, keeping the
    /// current route on screen until the new one lands — [`Self::reroute_from`]'s
    /// other half, for the end that moves when the destination is a person.
    pub fn retarget(&mut self, lat: f64, lng: f64) {
        let previous = self.route.take();
        let label = std::mem::take(&mut self.to.label);
        self.to.set(label, (lat, lng));
        self.invalidate();
        self.held = previous;
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

    /// Keep a start that reads "My position" at the position, once that is the
    /// avatar's.
    ///
    /// `default_start` fills the start once and never again, and the planner
    /// keeps its endpoints when it closes. So "My position" stayed wherever the
    /// camera was the first time the sheet opened: after a search had flown the
    /// map to Winsum and back, "My position → Winsum" refused as "the same place"
    /// from the far side of Groningen (2026-11-02). Only a start still wearing
    /// [`HERE_LABEL`] follows, and only once it is more than
    /// [`HERE_REFRESH_M`] out, so a person walking re-solves every few dozen
    /// metres rather than every frame. The caller passes only the avatar's
    /// position: refreshed from the camera, every pan of the map to look at the
    /// route would re-solve it.
    pub fn follow_here(&mut self, here: (f64, f64)) -> bool {
        if self.from.label != HERE_LABEL {
            return false;
        }
        let Some(at) = self.from.coord else {
            return false;
        };
        if cartopolis_core::geo::plane_distance_m([at.0, at.1], [here.0, here.1]) <= HERE_REFRESH_M
        {
            return false;
        }
        self.from.coord = Some(here);
        self.invalidate();
        true
    }

    /// One end, to read.
    #[must_use]
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub fn waypoint(&self, end: Endpoint) -> &Waypoint {
        match end {
            Endpoint::From => &self.from,
            Endpoint::To => &self.to,
        }
    }

    /// The detent the sheet is drawn at ([`effective_detent`]).
    #[must_use]
    pub fn detent(&self) -> SheetDetent {
        effective_detent(self.sheet, self.route.is_some(), self.picking.is_some())
    }

    /// The start is a field rather than a line: it is being changed, or it is
    /// still empty.
    #[must_use]
    pub fn start_open(&self) -> bool {
        self.from_editing || self.from.coord.is_none()
    }

    /// The end the fill chips under the card are for, if any: the start while
    /// it is open, the destination while it is empty, neither while a map
    /// pick is armed.
    #[must_use]
    pub fn fill_end(&self) -> Option<Endpoint> {
        if self.picking.is_some() {
            None
        } else if self.start_open() {
            Some(Endpoint::From)
        } else if self.to.label.trim().is_empty() {
            Some(Endpoint::To)
        } else {
            None
        }
    }

    /// The start reads "My position" and is placed: the "My position" chip
    /// has nothing to add.
    #[must_use]
    pub fn start_is_here(&self) -> bool {
        self.from.label == HERE_LABEL && self.from.coord.is_some()
    }

    /// A field's text, as a field's `changed()` hands it: the end is
    /// unresolved until its lookup answers or a place is chosen.
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub fn edit_label(&mut self, end: Endpoint, text: String) {
        self.waypoint_mut(end).label = text;
        self.label_edited(end);
    }

    /// What follows an edit of a field's text, whoever drew the field.
    fn label_edited(&mut self, end: Endpoint) {
        let wp = self.waypoint_mut(end);
        wp.coord = None;
        wp.last_edit = Some(Instant::now());
        if end == Endpoint::To {
            self.following = None;
        }
        self.invalidate();
    }

    /// Close a field's suggestions (Escape).
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub fn dismiss_suggestions(&mut self, end: Endpoint) {
        let wp = self.waypoint_mut(end);
        if wp.show_suggestions {
            wp.show_suggestions = false;
        }
    }

    /// The quiet line under a solved route's time: its distance, "to the
    /// door" when it ends at one, and where it goes.
    #[must_use]
    pub fn destination_line(&self, r: &Route) -> String {
        let door = if self.door.is_some() && self.door == self.to.coord {
            " · to the door"
        } else {
            ""
        };
        let to = self.to.label.trim_start_matches(PICKED_PREFIX).trim();
        if to.is_empty() {
            format!("{}{door}", format_distance(r.distance_m))
        } else {
            format!("{}{door} · to {to}", format_distance(r.distance_m))
        }
    }

    /// The start as its one line reads at rest: "My position", or the place,
    /// without the map-pick mark.
    #[must_use]
    pub fn start_text(&self) -> String {
        if self.from.label == HERE_LABEL {
            HERE_LABEL.to_string()
        } else {
            self.from
                .label
                .trim_start_matches(PICKED_PREFIX)
                .to_string()
        }
    }

    /// Apply everything the sheet asked for in one frame, except starting a
    /// trip ([`start_trip`]).
    pub fn apply(
        &mut self,
        actions: &PanelActions,
        nav: &mut GeoNavState,
        here: Option<(f64, f64)>,
    ) {
        if let Some(mode) = actions.mode {
            self.select_mode(mode);
        }
        // Any hand on the destination field, or a swap or Clear, is a different
        // destination from the person being followed.
        let touched_to = [
            actions.edited,
            actions.cleared,
            actions.chose.as_ref().map(|c| c.0),
        ]
        .contains(&Some(Endpoint::To));
        if touched_to || actions.swap || actions.clear {
            self.following = None;
        }
        if let Some(end) = actions.edited {
            self.label_edited(end);
        }
        if let Some(end) = actions.cleared {
            self.waypoint_mut(end).clear();
            self.invalidate();
            self.status = SolveStatus::Idle;
        }
        if let Some((end, s)) = actions.chose.clone() {
            self.waypoint_mut(end).set(s.label, (s.lat, s.lng));
            self.invalidate();
            // A start with a place in it folds back to its line.
            if end == Endpoint::From {
                self.from_editing = false;
            }
        }
        if let (Some(end), Some(here)) = (actions.use_here, here) {
            self.waypoint_mut(end).set(HERE_LABEL, here);
            self.invalidate();
            // Setting the start to "here" is the one thing the footnote form does
            // best; there is nothing left to type, so fold it back.
            if end == Endpoint::From {
                self.from_editing = false;
            }
        }
        if let Some(end) = actions.pick {
            self.pick_on_map(end);
        }
        if actions.swap {
            std::mem::swap(&mut self.from, &mut self.to);
            self.invalidate();
        }
        if actions.edit_start {
            self.from_editing = true;
        }
        if actions.collapse_start {
            self.from_editing = false;
        }
        if actions.clear {
            self.from.clear();
            self.to.clear();
            self.from_editing = false;
            self.invalidate();
            self.status = SolveStatus::Idle;
        }
        if let Some(i) = actions.goto_step {
            if let Some(step) = self.route.as_ref().and_then(|r| r.steps.get(i)) {
                nav.pending_teleport = Some(Teleport::geo(step.at[0], step.at[1], Some(220.0)));
            }
        }
    }
}

/// Start along the solved route: the planner gets out of the way, since
/// driving takes the whole screen and walking puts the camera behind the
/// avatar. Nothing without a route.
pub fn start_trip(
    trip: Trip,
    route: &RouteState,
    driver: &mut crate::systems::driver::DriverState,
    walk_guide: &mut crate::systems::walk_guide::WalkGuide,
    camera_state: &mut crate::resources::CameraState,
    right_panel: &mut RightPanel,
) {
    if route.route.is_none() {
        return;
    }
    match trip {
        // The turn list is on the manoeuvre card now.
        Trip::Drive => {
            walk_guide.stop();
            driver.enter();
        }
        Trip::Walk { auto } => {
            walk_guide.start(auto);
            camera_state.mode = crate::resources::CameraMode::Avatar;
        }
    }
    *right_panel = RightPanel::None;
}

/// How far the avatar walks before a "My position" start follows it
/// ([`RouteState::follow_here`]).
const HERE_REFRESH_M: f64 = 30.0;

/// What the start field says when it is the user's own position. Compared against
/// as well as written, so `follow_here` can refresh the coordinate after the
/// avatar has moved without stamping over a place the user typed.
pub const HERE_LABEL: &str = "My position";

// ─── Geocoding the endpoint fields ───────────────────────────────────────────

/// How far a followed person has to move before the route is re-solved to them.
///
/// A share arrives at most every 10 s and only after 15 m of movement
/// (`private_chat::share_due`), so a smaller number would re-solve on every
/// update of somebody walking toward you; at 50 m the line still ends where they
/// are, give or take a building.
const FOLLOW_RESOLVE_M: f64 = 50.0;

/// Keep a route to a person pointed at them: when their shared location has
/// moved [`FOLLOW_RESOLVE_M`] from the destination, the destination moves too.
///
/// When their share ends, the route stays where they last were — that is still
/// the best answer anyone has, and they may well be standing there.
/// A walk to a building ends at its door (`docs/mockups/routing/v1`, frame D).
///
/// A destination from a search, a saved place or a tap on a building is
/// usually the building's middle, so a walking route used to end at whichever
/// path was nearest to that point — often the back of the block. When the
/// destination lies *inside* a building the register has doors for, it moves to
/// the nearest of them ([`BuildingFacts::door_for`]) and the route re-solves to
/// it. Only on foot: a car's route ends at the road either way. Never for a
/// person being followed, and never for anything outside a building — a square,
/// a park, a meet-up — which keeps its own spot.
///
/// The register streams around the camera, and the first solve frames the
/// route, which brings the destination's cells in; so this re-checks whenever
/// more of the register has loaded, until the destination has been decided.
pub fn snap_walk_to_door(
    mut route: ResMut<RouteState>,
    facts: Res<crate::systems::building_facts::BuildingFacts>,
    mut decided: Local<Option<(f64, f64)>>,
    mut tried: Local<Option<((f64, f64), usize)>>,
) {
    if route.mode != TravelMode::Foot || route.following.is_some() {
        return;
    }
    let Some(to) = route.to.coord else {
        return;
    };
    // Settled for this destination; or asked already, against the same register.
    if *decided == Some(to) || *tried == Some((to, facts.len())) {
        return;
    }
    *tried = Some((to, facts.len()));
    match facts.door_for(to.0, to.1) {
        Some(door) => {
            *decided = Some(door);
            if cartopolis_core::geo::plane_distance_m([to.0, to.1], [door.0, door.1]) > 1.0 {
                tracing::info!(?door, "walking destination moved to the building's door");
                route.door = Some(door);
                let previous = route.route.take();
                let label = std::mem::take(&mut route.to.label);
                route.to.set(label, door);
                route.invalidate();
                route.held = previous;
            } else {
                route.door = Some(to);
            }
        }
        // Inside nothing that has loaded. Decided only once the register around
        // it is here, or a walk planned before the cells arrive never snaps.
        None if !facts.near(to.0, to.1, 300.0).is_empty() => *decided = Some(to),
        None => {}
    }
}

pub fn follow_shared_location(
    mut route: ResMut<RouteState>,
    chat: Res<crate::systems::chat::ChatState>,
) {
    let Some((channel, who)) = route.following.as_ref() else {
        return;
    };
    let Some(l) = chat
        .locations
        .iter()
        .find(|l| &l.channel == channel && &l.from == who)
    else {
        return;
    };
    let Some(to) = route.to.coord else {
        return;
    };
    let moved = cartopolis_core::geo::plane_distance_m([to.0, to.1], [l.latitude, l.longitude]);
    if moved > FOLLOW_RESOLVE_M {
        let (lat, lng) = (l.latitude, l.longitude);
        tracing::info!(
            moved_m = moved,
            "the person being routed to moved; re-solving"
        );
        route.retarget(lat, lng);
    }
}

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
    spawn_place_name(lat, lng, move |name| {
        if let (Some(name), Ok(mut g)) = (name, sink.lock()) {
            g.push((end, name));
        }
    });
}

/// Ask the geocoder what a point is called, and hand the answer (or `None`) to
/// `done` — the first three parts of its address, which is a street, a number
/// or a place, and a district.
pub(crate) fn spawn_place_name(
    lat: f64,
    lng: f64,
    done: impl FnOnce(Option<String>) + Send + 'static,
) {
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
            done(name);
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
    harness: Option<Res<crate::resources::ShotRun>>,
    time: Res<Time>,
    mut idle_s: Local<f32>,
    mut framed_for: Local<Option<((f64, f64), (f64, f64))>>,
    driver: Res<crate::systems::driver::DriverState>,
    walk: Res<crate::systems::walk_guide::WalkGuide>,
) {
    // 0. Age the graph cache — including while a solve is in flight, because the
    //    worker holds its own `Arc` clones and writes the slot back when it
    //    finishes, so releasing it early costs at worst the reuse this cache was
    //    for. Every path that starts or finishes a solve resets the timer below.
    //
    //    **Not while the route is being followed.** Three quiet minutes on the
    //    route is an ordinary stretch of any drive, and the first missed turn
    //    after it then found the cache gone and refetched and rebuilt the whole
    //    corridor — seconds on a phone, in the one moment the held route was
    //    covering for — where a warm cache answers it with one A*.
    let following = driver.active || walk.active;
    if following {
        *idle_s = 0.0;
    }
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
                    route.by_mode = std::iter::once(r.clone()).chain(job.others).collect();
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
                    route.status = SolveStatus::Failed(failure_message(&route, &e));
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
    // A reroute is for the mode being followed, and the times of the other two
    // are for a planner nobody has open: searching them first held every car
    // reroute behind a walking A* over the whole corridor — the slowest of the
    // three, its heuristic the weakest. Their chips go blank until the next
    // ordinary solve, which is the honest state for a route that has moved.
    let with_others = route.held.is_none();
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
                Err(e) => publish_solve(&sink, seq, Err(e), Vec::new()),
                Ok(input) => {
                    // Somebody tapped for this and is watching a spinner, so it
                    // goes ahead of the map cells — behind only the basemap.
                    crate::platform::workers::submit_ranked(
                        "routing",
                        crate::platform::workers::NEAR,
                        Box::new(move || {
                            let result = build_and_search(input, from, to, mode, &cache);
                            let others = if result.is_ok() && with_others {
                                other_modes(&cache, from, to, mode)
                            } else {
                                Vec::new()
                            };
                            publish_solve(&sink, seq, result, others);
                        }),
                    );
                }
            }
        })
        .detach();
}

/// What the planner says about a failed solve.
///
/// The router's own words, except where the planner knows more than the router
/// does. "Start and destination are the same place" under "My position →
/// Winsum" read as a broken router; saying how far apart the two ends are
/// shows which of them is wrong.
fn failure_message(route: &RouteState, e: &RouteError) -> String {
    let RouteError::TooClose = e else {
        return e.to_string();
    };
    let to = route.to.label.trim_start_matches(PICKED_PREFIX).trim();
    let apart = route
        .from
        .coord
        .zip(route.to.coord)
        .map(|(a, b)| cartopolis_core::geo::plane_distance_m([a.0, a.1], [b.0, b.1]));
    match apart {
        Some(m) if route.from.label == HERE_LABEL && !to.is_empty() => {
            format!("You are already at {to} ({} away)", format_distance(m))
        }
        Some(m) => format!(
            "Start and destination are {} apart, on the same stretch of road",
            format_distance(m)
        ),
        None => e.to_string(),
    }
}

/// Where a detached solve leaves its answer: empty until it lands.
pub(crate) type DetachedSolve = Arc<Mutex<Option<Result<Route, RouteError>>>>;

/// Solve a route for something other than the Route panel: an agent walking the
/// avatar along streets rather than through buildings (`mcp::agent`), or the
/// walk between two friends a meet-up is placed halfway along (`meetup`).
///
/// The same fetch, graph build and search the panel runs, with a cache and a
/// progress counter of its own — so it neither replaces a route somebody has on
/// screen nor waits behind one. A raster basemap has no geometry to route on and
/// answers `NoData` at once.
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
fn publish_solve(
    sink: &Mutex<Option<SolveJob>>,
    seq: u64,
    result: Result<Route, RouteError>,
    others: Vec<Route>,
) {
    if let Ok(mut g) = sink.lock() {
        *g = Some(SolveJob {
            seq,
            result,
            others,
        });
    }
}

/// Route the same endpoints for every travel mode but `solved`, over the graph
/// the solve just left in the cache.
///
/// Only over a direct corridor. A staged one is a coarse long-haul graph with a
/// street-level patch at each end, and walking or cycling a few hundred
/// kilometres is not a question the chips need to answer — nor one worth an A*
/// over the whole coarse graph at walking speed, whose heuristic is too weak to
/// prune anything. A mode with no route (a motorway-only corridor on foot) is
/// simply absent, and its chip shows no time.
fn other_modes(
    cache: &GraphCache,
    from: (f64, f64),
    to: (f64, f64),
    solved: TravelMode,
) -> Vec<Route> {
    let graph = cache.lock().ok().and_then(|g| match g.as_ref() {
        Some((_, RouteGraphs::Direct(graph))) => Some(graph.clone()),
        _ => None,
    });
    let Some(graph) = graph else {
        return Vec::new();
    };
    TravelMode::ALL
        .into_iter()
        .filter(|m| *m != solved)
        .filter_map(|m| graph.route(from, to, m).ok())
        .collect()
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
    ui_input: Res<crate::systems::ui_input::UiInput>,
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
    // A screen of the library's under the pointer (its route sheet, whose
    // taps must not drop a pin on the map behind it).
    if ui_input.owns_click(false, None) {
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
    let cam_world = anchored_position_f64(cell, tf.translation);
    let hit = cam_world + dir * (cam_world.y / -dir.y);
    let (lat, lng) = world_to_lat_lng(anchor, hit.x, hit.z);

    route.set_endpoint_at(end, lat, lng);
    tracing::info!(?end, lat, lng, "route endpoint picked on the map");
}

// ─── World rendering ─────────────────────────────────────────────────────────

/// Materials for the route line, built once.
#[derive(Resource, Clone)]
pub struct RouteMaterials {
    fill: Handle<StandardMaterial>,
    casing: Handle<StandardMaterial>,
    start: Handle<StandardMaterial>,
    finish: Handle<StandardMaterial>,
    /// The white ring round each pin.
    rim: Handle<StandardMaterial>,
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
    mut drawn: Local<Option<(u32, u32, u32, u32, u32, bool)>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    decks: Res<crate::systems::map_geometry::DeckPlates>,
    globe: Res<GlobeState>,
    cameras: AnchoredCamera,
    driver: Res<crate::systems::driver::DriverState>,
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
    let width_scale = cameras.get().map_or(1.0, |(cell, tf)| {
        route_width_scale(globe.altitude_m(cell, tf) as f32)
    });
    // Never the footpath band while driving. The 3D chase rig tightens onto a
    // junction to ~41 m up, a metre over `STREET_LEVEL_ALT`, so a turn could flip
    // the line between road and footpath width — a whole-route rebuild each way,
    // mid-manoeuvre, for a line a driver needs at road width throughout.
    let width_scale = if driver.active {
        width_scale.max(1.0)
    } else {
        width_scale
    };
    // Only a route that climbs onto a deck reads the decks (`route_lift`), so only
    // such a route rebuilds when one lands. Keyed unconditionally, every deck cell
    // streaming in or out anywhere re-draped the whole line — a main-thread
    // rebuild and an upload burst for a route that crosses no bridge.
    let on_decks = route
        .displayed()
        .is_some_and(|r| r.levels.iter().any(|&l| l > 0));
    let key = (
        route.generation,
        stream.anchor_generation,
        terrain.revision,
        if on_decks { decks.revision } else { 0 },
        width_scale.to_bits(),
        // The start pin is not drawn while driving (below).
        driver.active,
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
        Some(p) => p.clone(),
        None => {
            let mut line = |r: f32, g: f32, b: f32, bias: f32| {
                materials.add(StandardMaterial {
                    base_color: Color::srgb(r, g, b),
                    unlit: true,
                    // Not hazed. The distance fog greys everything toward the sky
                    // colour with range, which is right for the city and wrong for
                    // the one mark on it that has to read from any height: a
                    // cross-town route planned from 4 km up came out as a pale
                    // thread a shade off the haze, and the pins vanished into it.
                    fog_enabled: false,
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
            let pin = GroundLayer::RoutePin.depth_bias();
            let p = RouteMaterials {
                fill: line(0.16, 0.53, 0.90, GroundLayer::RouteLine.depth_bias()),
                casing: line(0.06, 0.22, 0.42, GroundLayer::RouteCasing.depth_bias()),
                start: line(
                    ROUTE_START_RGB[0],
                    ROUTE_START_RGB[1],
                    ROUTE_START_RGB[2],
                    pin,
                ),
                finish: line(
                    ROUTE_FINISH_RGB[0],
                    ROUTE_FINISH_RGB[1],
                    ROUTE_FINISH_RGB[2],
                    pin,
                ),
                rim: line(1.0, 1.0, 1.0, pin),
            };
            commands.insert_resource(p.clone());
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

    // `width_scale` is in the redraw key, so the street-level height follows the
    // same band switch as the street-level width.
    let lift_y = if width_scale < 1.0 {
        STREET_LEVEL_Y
    } else {
        ROUTE_Y
    };
    let base = DVec3::new(origin.x, f64::from(lift_y), origin.z);
    // Each layer of the line floats this far over the one below it. The depth
    // rungs alone cannot hold the fill above its casing: both are *draped*, the
    // fill's vertices sample the ground at different points from the casing's,
    // and wherever the ground bends between them the fill came out centimetres
    // under its own casing — the mottled dark-and-light line of the 3D chase view.
    // A lift that grows with the width is hundreds of depth quanta or more at the
    // height the line is looked at from (the 64-unit rungs are not), so it ends
    // the fight instead of refereeing it.
    let layer_lift = f64::from(LAYER_LIFT_M * width_scale.max(0.3));
    spawn(
        ribbon(&road, &road_lift, CASING_HALF_WIDTH * width_scale),
        palette.casing.clone(),
        base,
    );
    spawn(
        ribbon(&road, &road_lift, ROUTE_HALF_WIDTH * width_scale),
        palette.fill.clone(),
        base + DVec3::Y * layer_lift,
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

    // Start / finish, at the coordinates the user actually asked for: a coloured
    // disc in a white ring, over the line. A bare disc a shade wider than the line
    // was lost at any height a route is planned from — the start a red speck, the
    // finish not there at all against the haze — and a ring is what every map
    // draws a pin's foot as, because it separates the mark from whatever it lies on.
    //
    // No start pin while driving: the vehicle is there, and the green disc lay
    // under the arrow like a puddle for the first minute of every drive.
    let pins = [
        (!driver.active).then_some((from_pin, palette.start)),
        Some((to_pin, palette.finish)),
    ];
    // From above the city the pins ride over the roofs. A pin is where the user
    // pointed, which is as often a building as a street: on the default start by
    // the Grote Markt the disc was drawn, counted as rendered, and entirely under
    // a roof from 4.5 km up. Only in the widened bands, where the camera is high
    // enough that the rise is no parallax at all; at street and chase height the
    // pin stays on the ground it marks.
    let rise = if width_scale > 1.0 {
        f64::from(PIN_RISE_M * width_scale)
    } else {
        0.0
    };
    for (point, material) in pins.into_iter().flatten() {
        let at = base
            + DVec3::new(
                f64::from(point.x),
                2.0 * layer_lift + rise,
                f64::from(point.y),
            );
        let radius = MARKER_RADIUS * width_scale;
        spawn(disc(radius * MARKER_RIM_SCALE), palette.rim.clone(), at);
        spawn(disc(radius), material, at + DVec3::Y * layer_lift);
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
/// reason.** A compact right panel *is* the viewport, so asking for a route to somewhere
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
/// Most of the viewport height the sheet may claim. The rest is the point of the
/// exercise.
const SHEET_MAX_FRACTION: f32 = 0.55;
/// The sheet never shrinks below this, even on a viewport too short to honour
/// [`SHEET_MAX_FRACTION`]. The rule a narrow control follows here:
/// overflow visibly rather than collapse a control to nothing.
const SHEET_BODY_MIN_PTS: f32 = 96.0;

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
/// The keys' reach ([`KeyboardInset`](crate::systems::onscreen_keyboard::KeyboardInset))
/// is how far they stand up from the bottom edge — the slab plus its edge
/// inset — and 0 while they are down or docked to the top, which is when the
/// dock is what to clear.
fn sheet_bottom(keyboard_reach: f32) -> f32 {
    if keyboard_reach > 0.0 {
        return keyboard_reach + theme::OSD_GAP;
    }
    crate::systems::dock::reserved_bottom() + theme::OSD_MARGIN
}

/// What Start does for a mode — its word, or what is missing for it.
///
/// Walking happens in the world (`systems::walk_guide`), so it needs the avatar,
/// which only a session has. Riding and driving are navigation along a real road
/// with the device in your hand: driver mode follows the position fix, and on a
/// device with no location source it would be narrating an avatar nobody is
/// moving — so the button says what is missing instead.
pub(crate) fn start_action(
    mode: TravelMode,
    gps: bool,
    avatar: bool,
) -> Result<&'static str, &'static str> {
    match mode {
        TravelMode::Foot if avatar => Ok("Walk"),
        TravelMode::Foot => Err("Sign in to walk it"),
        TravelMode::Bike if gps => Ok("Ride"),
        TravelMode::Car if gps => Ok("Drive"),
        TravelMode::Bike => Err("Riding follows your real position, which this device cannot give"),
        TravelMode::Car => Err("Driving follows your real position, which this device cannot give"),
    }
}

/// The route planner's state, each frame its sheet (`lib_route`) is up:
/// where "My position" is, the start kept on the avatar, the fetch progress,
/// what the sheet needs from the avatar, the camera and the keyboard
/// ([`RouteState::lib_view`]), the trip it asked to start
/// ([`RouteState::lib_trip`]), and where the sheet is for the pins
/// ([`RouteState::sheet_rect`]).
#[allow(clippy::too_many_arguments)]
pub fn drive_route_sheet(
    metrics: Res<crate::systems::ui_input::UiMetrics>,
    mut route: ResMut<RouteState>,
    mut right_panel: ResMut<RightPanel>,
    mut driver: ResMut<crate::systems::driver::DriverState>,
    mut walk_guide: ResMut<crate::systems::walk_guide::WalkGuide>,
    mut camera_state: ResMut<crate::resources::CameraState>,
    bodies: Query<(), With<crate::components::Avatar>>,
    avatar: Res<crate::resources::AvatarState>,
    keyboard: Option<Res<crate::systems::onscreen_keyboard::KeyboardInset>>,
    globe: Res<GlobeState>,
    camera: AnchoredCamera,
    map_anchor: Res<MapAnchor>,
    clock: Res<crate::systems::transit_live::WorldClock>,
) {
    if !right_panel.is(RightPanel::Route) {
        // Read before writing: an unconditional store would mark `RouteState`
        // changed on every frame of a session that never opens the planner.
        if route.sheet_rect.is_some() {
            route.sheet_rect = None;
        }
        return;
    }

    // Where "My position" resolves to: the avatar, when there is one — it is
    // where the person is in the world, and a device fix moves it there — and
    // the camera only with no avatar to ask (offline, a scripted capture).
    let avatar_here = map_anchor.geo.filter(|_| !bodies.is_empty()).map(|anchor| {
        let p = avatar.position;
        world_to_lat_lng(anchor, f64::from(p.x), f64::from(p.z))
    });
    let here = avatar_here.or_else(|| {
        camera
            .get()
            .zip(map_anchor.geo)
            .map(|((cell, tf), anchor)| camera_geo(anchor, &globe, cell, tf))
    });

    // Fill the start in the moment the panel can see one, so the user only ever
    // has a destination to supply; and keep it on the avatar as it walks. Not
    // while a route is being followed: the driver and the walk guide move the
    // start themselves, through `reroute_from`.
    if !route.default_start(here) && !driver.active && !walk_guide.active {
        if let Some(at) = avatar_here {
            route.follow_here(at);
        }
    }

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

    let viewport = metrics.viewport_pts();
    let bottom = sheet_bottom(keyboard.map_or(0.0, |k| k.0));

    // Hand the sheet what only this system can see, start the trip it asked
    // for, and publish where it is for the pins. A click on the sheet is held
    // off the world by the pointer's ownership (`lib_route`).
    let arrive = route.route.as_ref().map(|r| {
        let ((h, m, _), local) = clock.hms();
        crate::systems::driver::arrival_clock(h, m, r.duration_s, local)
    });
    let view = RouteView {
        here,
        gps: LocationWatch::available(),
        avatar: !bodies.is_empty(),
        bottom,
        viewport: [viewport.x, viewport.y],
        arrive,
    };
    if route.lib_view != view {
        route.lib_view = view;
    }
    if let Some(trip) = route.lib_trip.take() {
        start_trip(
            trip,
            &route,
            &mut driver,
            &mut walk_guide,
            &mut camera_state,
            &mut right_panel,
        );
    }
    // Estimated, not measured: `bevy_ui` lays the sheet out after this runs.
    // At its peek the sheet is about the header, the modes, the summary and
    // the actions; open it is at most its cap.
    let width = lib_sheet_width(viewport.x);
    let max_h = lib_sheet_max_height(viewport.y, bottom);
    let height = match route.detent() {
        SheetDetent::Full => max_h,
        SheetDetent::Peek => LIB_SHEET_PEEK_PTS.min(max_h),
    };
    let rect = Rect::new(
        (viewport.x - width) / 2.0,
        viewport.y - bottom - height,
        (viewport.x + width) / 2.0,
        viewport.y - bottom,
    );
    if route.sheet_rect != Some(rect) {
        route.sheet_rect = Some(rect);
    }
}

/// The start button's glyph and words, apart: the sheet sets the glyph in
/// the icon font.
pub(crate) fn start_parts(
    mode: TravelMode,
    gps: bool,
    avatar: bool,
) -> (&'static str, &'static str) {
    match start_action(mode, gps, avatar) {
        Ok(_) if mode == TravelMode::Foot => (icons::PLAY, "Walk me there"),
        Ok(verb) => (icons::PLAY, verb),
        Err(missing) => {
            let icon = if mode == TravelMode::Foot {
                icons::WALK
            } else {
                icons::HERE
            };
            (icon, missing)
        }
    }
}

/// What the sheet (`lib_route`) asked for this frame, applied by
/// [`RouteState::apply`].
#[derive(Default, Clone)]
pub struct PanelActions {
    pub swap: bool,
    pub clear: bool,
    pub mode: Option<TravelMode>,
    pub goto_step: Option<usize>,
    /// Endpoint set to the current position.
    pub use_here: Option<Endpoint>,
    /// Endpoint armed for a map click.
    pub pick: Option<Endpoint>,
    /// Endpoint resolved from a suggestion.
    pub chose: Option<(Endpoint, GeoSuggestion)>,
    /// Endpoint whose text changed (invalidates its coordinate).
    pub edited: Option<Endpoint>,
    pub cleared: Option<Endpoint>,
    /// Open the start as an editable field / fold it back to a footnote.
    pub edit_start: bool,
    pub collapse_start: bool,
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
mod tests;
