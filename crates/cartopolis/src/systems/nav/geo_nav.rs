//! Geo navigation: a browser-style search, in a panel of its own
//! ([`render_search_panel`], the dock's leftmost disc or `Ctrl+L`).
//!
//! Type a place name ("Groningen") to get OpenStreetMap Nominatim suggestions
//! and click one to teleport the avatar/camera there; or paste raw
//! coordinates — "lat, lng" or "lat, lng, altitude" (the format the HUD's
//! copy button produces) — to jump directly without a lookup. Teleports keep
//! the current camera mode; bookmark jumps and the last-location restore
//! (`user_store`) go through the same [`GeoNavState::pending_teleport`] path.
//!
//! Requests are debounced behind a short typing pause, answered from a small
//! per-session cache when we have asked before, and otherwise sent with up to
//! [`MAX_IN_FLIGHT`] outstanding so a slow reply never holds up the query the
//! user is actually typing. They stay rate-limited to the endpoint's policy —
//! near-live against the self-hosted instance, ≥1 s against the public API
//! (https://operations.osmfoundation.org/policies/nominatim/) — and are sent
//! with an identifying User-Agent.

use crate::components::FreeCamera;
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use web_time::Instant;

use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::{Grid, GridCell};
use serde::{Deserialize, Serialize};

use crate::components::Avatar;
use crate::platform::http;
use crate::resources::{AvatarState, CameraMode, CameraState, RightPanel};
use crate::systems::egui_manager::EguiManager;
use crate::systems::icons;
use crate::systems::map_stream::{lat_lng_to_tile_frac, MapStream};
use crate::systems::navigation::{Endpoint, RouteState};
use crate::systems::theme;
use cartopolis_core::world::tile_to_meters;

/// Nominatim base URL. Defaults to the project's self-hosted instance (seeded with
/// the Netherlands extract — see `docs/guides/server.md`). Override with
/// `CARTO_NOMINATIM_URL`, e.g. the public `https://nominatim.openstreetmap.org`.
const DEFAULT_NOMINATIM_URL: &str = "https://nominatim.cartopolis.org";

/// A teleport this close to the map anchor moves the camera in the frame it is
/// in rather than re-anchoring (`apply_teleport`). The near field is flat around
/// the anchor while the basemap follows the Earth's curve, and at the far edge of
/// the ground ring from a camera this far out they part by ~2 m (d²/2R at ~5 km).
pub const SHORT_JUMP_M: f64 = 3_000.0;

/// Resolve the Nominatim base URL (no trailing slash) from `CARTO_NOMINATIM_URL`,
/// falling back to the self-hosted default. Callers append `/search`, `/reverse`, …
pub(crate) fn nominatim_base_url() -> String {
    std::env::var("CARTO_NOMINATIM_URL")
        .unwrap_or_else(|_| DEFAULT_NOMINATIM_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}
/// Typing pause before a query is sent — short so suggestions feel near-live.
///
/// Measured 2026-07-31, warm, against the self-hosted instance: ~70 ms end to end
/// (~5–100 ms server, the rest network). At the old 120 ms this pause was the
/// single largest term in the budget — the client was slower than the round trip
/// it was pacing. One frame either way is imperceptible; a fifth of a second is
/// not. See `docs/notes/geocoding-latency.md`.
const DEBOUNCE: Duration = Duration::from_millis(60);
/// Minimum spacing between requests. The self-hosted instance has no rate policy
/// (near-live); the public API allows at most one request per second, so stay
/// polite when `CARTO_NOMINATIM_URL` points back at `nominatim.openstreetmap.org`.
static MIN_REQUEST_INTERVAL: LazyLock<Duration> = LazyLock::new(|| {
    if nominatim_base_url().contains("openstreetmap.org") {
        Duration::from_secs(1)
    } else {
        Duration::from_millis(40)
    }
});
/// How many lookups may be outstanding at once.
///
/// This used to be 1, which made a single slow reply stall *every* later
/// keystroke: the reply for "Gron" had to land before "Groningen" could even be
/// sent, so one cold query (seconds, before the server move) added its whole
/// latency to the next query the user actually cared about. Replies are already
/// tagged with the query that produced them and stale ones are dropped, so
/// letting a newer query overtake an older one is free — the cap only exists so
/// a burst of keystrokes cannot open an unbounded number of sockets.
const MAX_IN_FLIGHT: u32 = 3;
/// Remembered lookups, newest last. Small and linearly scanned — retyping or
/// backspacing through a query costs nothing at all, which is the case the
/// debounce cannot help with.
const CACHE_CAP: usize = 64;
const MIN_QUERY_LEN: usize = 2;
const MAX_SUGGESTIONS: usize = 8;
/// How long an empty answer has to stand before the dropdown says "No results".
///
/// Nominatim does no prefix matching at all — it looks whole word tokens up by
/// equality — so *every* keystroke on the way to a full name answers `[]`
/// ("Groning" returns nothing; "Groningen" returns three). Reporting that
/// immediately meant the dropdown announced failure between keystrokes, on the
/// query the user had not finished typing yet. The grace is a little longer than
/// a comfortable typing interval, so the message only appears once typing has
/// actually stopped. See `docs/notes/geocoding-latency.md`.
const NO_RESULTS_GRACE: Duration = Duration::from_millis(600);
/// How many previously visited places the empty Search panel offers.
const RECENT_SUGGESTIONS: usize = 6;
/// Avatar rest height after a teleport. The humanoid models are authored
/// origin-at-feet, so this is the ground/tile surface (`avatar::GROUND_HEIGHT`);
/// the offline gravity clamp rests the feet at the same value. (Was 0.45 = ground
/// + half fox height, which floated the humanoids ~0.4 m.)
const AVATAR_GROUND_Y: f64 = 0.05;
/// Minimum free-camera altitude after a teleport that had to **choose** one, so
/// the destination is visible. Not a floor on an altitude the caller stated —
/// see the `CameraMode::Free` arm of `apply_teleport` for why the two differ.
pub const MIN_TELEPORT_ALTITUDE: f64 = 60.0;
/// Floor on an *explicit* free-camera altitude: enough to keep the near plane
/// out of the ground and the view clear of head height, and nothing more.
pub const MIN_CAMERA_ALTITUDE: f64 = 2.0;

/// Where a free-camera teleport lands, in metres above the anchor's ground.
///
/// Split out of `apply_teleport` so the rule is testable without a `World`, and
/// because the rule is the whole subtlety: a *stated* altitude takes only the
/// keep-out-of-the-ground floor, a *derived* one takes the see-the-destination
/// floor. `current` is a closure because reading it costs a grid conversion the
/// stated-altitude path has no use for.
fn free_teleport_altitude(target: Option<f64>, current: impl FnOnce() -> f64) -> f64 {
    match target {
        Some(alt) => alt.max(MIN_CAMERA_ALTITUDE),
        None => current().max(MIN_TELEPORT_ALTITUDE),
    }
}

/// One hit in a geocoding dropdown.
///
/// Nominatim answers with a single `display_name` comma-run — "Amsterdam, North
/// Holland, Netherlands" — which is the whole address flattened into a line. Shown
/// as one truncated string it hides exactly the part that tells two hits apart:
/// the eight Amsterdams the public index returns differ only past the first comma.
/// So the hit is split at parse time into what it *is* ([`name`](Self::name)), what
/// disambiguates it ([`context`](Self::context)) and what kind of thing it is
/// ([`kind`](Self::kind)); `label` stays the full line, because that is what goes
/// back into the search field when the hit is picked.
#[derive(Clone)]
pub struct GeoSuggestion {
    /// The full `display_name` — the text a picked suggestion leaves in the field.
    pub label: String,
    /// Primary name ("Amsterdam"), the first line of a suggestion row.
    pub name: String,
    /// The rest of the address ("North Holland, Netherlands"), the second line.
    pub context: String,
    /// Nominatim's `addresstype` ("city", "island", "road"), shown beside the name.
    pub kind: Option<String>,
    pub lat: f64,
    pub lng: f64,
}

impl GeoSuggestion {
    /// Parse one element of a Nominatim `format=jsonv2` array. Shared by the
    /// address bar and the route panel, which ask the same endpoint the same way
    /// and had drifted into two copies of this that read different fields.
    pub fn from_nominatim(it: &serde_json::Value) -> Option<Self> {
        let label = it.get("display_name")?.as_str()?.to_string();
        let lat = it.get("lat")?.as_str()?.parse().ok()?;
        let lng = it.get("lon")?.as_str()?.parse().ok()?;
        // `name` is jsonv2's own primary label. It is missing or empty for plenty
        // of hits (an unnamed way, a house number), so fall back to splitting the
        // display line at its first comma — which is the same thing by convention.
        let name = it
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let (name, context) = match name {
            Some(n) => {
                let rest = label
                    .strip_prefix(&n)
                    .map(|r| r.trim_start_matches([',', ' ']).to_string())
                    .unwrap_or_else(|| label.clone());
                (n, rest)
            }
            None => match label.split_once(", ") {
                Some((head, rest)) => (head.to_string(), rest.to_string()),
                None => (label.clone(), String::new()),
            },
        };
        let kind = it
            .get("addresstype")
            .or_else(|| it.get("type"))
            .and_then(|v| v.as_str())
            .map(|s| s.replace('_', " "))
            .filter(|s| !s.is_empty() && s != "yes");
        Some(Self {
            label,
            name,
            context,
            kind,
            lat,
            lng,
        })
    }

    /// A suggestion with no address behind it — the "go to these coordinates"
    /// entry, and the fixture the tests use.
    pub fn plain(label: impl Into<String>, lat: f64, lng: f64) -> Self {
        let label = label.into();
        Self {
            name: label.clone(),
            label,
            context: String::new(),
            kind: None,
            lat,
            lng,
        }
    }
}

/// A finished Nominatim lookup, tagged with the query it answered so stale
/// replies (the user kept typing) can be dropped.
struct GeocodeReply {
    query: String,
    result: Result<Vec<GeoSuggestion>, String>,
}

/// A destination on its way to the route planner, from a row that cannot reach it.
///
/// Everything that names a place — a search hit, a saved place, the shop the
/// avatar is standing in front of — is a destination, and until this existed none
/// of them could be routed to: [`RouteState::set_endpoint_at`] had exactly two
/// callers, the Route panel's own fields and the map's context menu, so the only
/// way to navigate to something you had *found* was to fly there and long-press
/// the map. The three sources queue this instead, because none of them owns the
/// planner: the Places panel and the interaction prompt are drawn by systems that
/// do not take `RouteState`, and the Search panel is one panel arguing with
/// another about which of them owns the route. They all hold
/// [`GeoNavState`], which is already the client's "where do you want to go"
/// resource, so it carries the request and `apply_route_request` spends it.
pub struct RouteRequest {
    /// The name the user just read on the row they tapped.
    pub label: String,
    pub lat: f64,
    pub lng: f64,
    /// Which end of the route this place becomes. Almost always
    /// [`Endpoint::To`] — the place card's "Route from" is the one caller that
    /// plans *away* from a found place.
    pub end: Endpoint,
}

#[derive(Resource)]
pub struct GeoNavState {
    pub query: String,
    suggestions: Vec<GeoSuggestion>,
    show_suggestions: bool,
    /// A *failure* — a request that errored out. Not "no results", which is an
    /// answer and is timed (see [`empty_since`](Self::empty_since)).
    error: Option<String>,
    /// The suggestions on screen answer an earlier query than the one in the
    /// field. They stay up, dimmed, rather than being cleared — see
    /// [`apply_suggestions`](Self::apply_suggestions).
    stale: bool,
    /// When the current query first came back empty, or `None` if it has not.
    /// [`status`](Self::status) waits [`NO_RESULTS_GRACE`] on this.
    empty_since: Option<Instant>,
    /// Time of the last edit (debounce timer).
    last_edit: Option<Instant>,
    last_sent: String,
    last_sent_at: Option<Instant>,
    /// Lookups currently outstanding (see [`MAX_IN_FLIGHT`]).
    in_flight: u32,
    /// Query → its answer, newest last, capped at [`CACHE_CAP`].
    cache: Vec<(String, Vec<GeoSuggestion>)>,
    /// Enter was pressed before results arrived: jump to the top hit when they do.
    teleport_on_result: bool,
    /// Enter re-submits a query even if it equals the last sent one.
    force_search: bool,
    results: Arc<Mutex<Vec<GeocodeReply>>>,
    /// Target picked from a suggestion, pasted coordinates, a bookmark, or the
    /// startup restore. Consumed by `apply_teleport` (kept until the world
    /// anchor exists).
    pub pending_teleport: Option<Teleport>,
    /// The name to file the next teleport under in the history, when the caller
    /// has one. `None` leaves the entry named by its coordinates — which is all a
    /// startup restore or a globe descent can honestly say about itself.
    pub pending_label: Option<String>,
    /// A destination waiting to be handed to the route planner. See
    /// [`RouteRequest`]; spent by `apply_route_request`.
    pub route_request: Option<RouteRequest>,
    /// A place picked in the Search panel, waiting for the place card
    /// (`place_card::render_place_card`, which also closes the panel). The same
    /// queue shape as [`route_request`](Self::route_request), for the same
    /// reason: the row that was tapped is drawn by a panel system that does not
    /// own the card.
    pub pending_card: Option<crate::systems::place_card::CardPlace>,
    /// Browser-style location history; `history[history_pos]` is where we are.
    /// Every teleport that isn't itself a Back/Forward move truncates the
    /// forward entries and appends the destination (see `apply_teleport`).
    pub history: Vec<NavEntry>,
    pub history_pos: usize,
    /// The pending teleport is a Back/Forward move — `apply_teleport` must not
    /// re-record it as a new visit.
    from_history: bool,
    /// The search field should grab keyboard focus next frame (set by `Ctrl+L`
    /// and by [`seed_search`](Self::seed_search), consumed in [`search_ui`]);
    /// also *opens* the Search panel, since a field that is not drawn cannot take
    /// focus.
    pub focus_search: bool,
    /// The search field currently holds keyboard focus — read by
    /// `ui_owns_keyboard` so typing a search doesn't also walk the avatar (set in
    /// [`search_ui`]).
    pub search_has_focus: bool,
}

impl GeoNavState {
    /// A previously fetched answer for `query`, if we still hold one.
    fn cached(&self, query: &str) -> Option<Vec<GeoSuggestion>> {
        self.cache
            .iter()
            .find(|(q, _)| q == query)
            .map(|(_, list)| list.clone())
    }

    /// Remember an answer, evicting the oldest once [`CACHE_CAP`] is reached.
    /// Re-inserting an existing query moves it to the newest end.
    fn remember(&mut self, query: String, list: &[GeoSuggestion]) {
        self.cache.retain(|(q, _)| *q != query);
        self.cache.push((query, list.to_vec()));
        if self.cache.len() > CACHE_CAP {
            self.cache.remove(0);
        }
    }

    /// Put an answer on screen — shared by a network reply and a cache hit, so
    /// the two are indistinguishable to the user (including an Enter pressed
    /// before the results landed).
    ///
    /// **An empty answer does not clear the list.** The endpoint does no prefix
    /// matching, so a half-typed name is answered `[]` as a matter of course
    /// (`docs/notes/geocoding-latency.md`), and assigning that unconditionally
    /// made the dropdown blink empty between keystrokes — the search read as
    /// broken exactly while it was working. The last real list stays up instead,
    /// marked [`stale`](Self::stale) so the rows can be drawn dimmed, and the
    /// "No results" verdict is deferred to [`status`](Self::status).
    fn apply_suggestions(&mut self, list: Vec<GeoSuggestion>) {
        self.error = None;
        if list.is_empty() {
            self.empty_since.get_or_insert_with(Instant::now);
            // Enter is still spent even when there is nothing to jump to, or the
            // *next* answer to land would teleport on a keystroke long past.
            self.teleport_on_result = false;
            self.show_suggestions = true;
            if !self.suggestions.is_empty() {
                self.stale = true;
                return; // keep what is on screen
            }
            self.stale = false;
            self.suggestions = list;
            return;
        }

        self.empty_since = None;
        self.stale = false;
        if self.teleport_on_result {
            self.teleport_on_result = false;
            let first = &list[0];
            self.pending_teleport = Some(Teleport::geo(first.lat, first.lng, None));
            self.pending_label = Some(first.name.clone());
            self.query = first.label.clone();
            self.show_suggestions = false;
        } else {
            self.show_suggestions = true;
        }
        self.suggestions = list;
    }

    /// The line under the rows, if any: a failure at once, "No results" only once
    /// an empty answer has stood for [`NO_RESULTS_GRACE`].
    fn status(&self) -> Option<String> {
        if let Some(e) = &self.error {
            return Some(e.clone());
        }
        self.empty_since
            .filter(|t| t.elapsed() >= NO_RESULTS_GRACE)
            .map(|_| "No results".to_string())
    }

    /// Forget the answer on screen. Called when the field is emptied — a stale
    /// list under a blank query is not "the last thing you searched", it is a
    /// list of places with nothing tying them to what the bar now says.
    fn forget_suggestions(&mut self) {
        self.suggestions.clear();
        self.stale = false;
        self.empty_since = None;
    }

    /// Ask for a route to a named place, from a row that has no way to reach the
    /// planner itself. See [`RouteRequest`].
    pub fn route_to(&mut self, label: impl Into<String>, lat: f64, lng: f64) {
        self.route_request = Some(RouteRequest {
            label: label.into(),
            lat,
            lng,
            end: Endpoint::To,
        });
    }

    /// Ask for a route *starting* at a named place — the place card's "Route
    /// from". Same queue, other end.
    pub fn route_from(&mut self, label: impl Into<String>, lat: f64, lng: f64) {
        self.route_request = Some(RouteRequest {
            label: label.into(),
            lat,
            lng,
            end: Endpoint::From,
        });
    }

    /// The places visited this session, newest first, excluding the one we are
    /// standing on.
    ///
    /// This is a destination source in its own right, and the only one that needs
    /// no typing *and* no prior curation — which is what makes it worth surfacing
    /// on a phone, where the Back button is the only thing the history was
    /// reachable through and it can only walk one step at a time.
    fn recent_entries(&self) -> impl Iterator<Item = (usize, &NavEntry)> {
        let here = self.history_pos;
        self.history
            .iter()
            .enumerate()
            .rev()
            .filter(move |(i, _)| *i != here)
            .take(RECENT_SUGGESTIONS)
    }

    /// Drop the suggestion list, and any pending "fly to the top hit" with it.
    /// Called when the Search panel is closed: the list is the answer to a
    /// question the user has stopped asking, and a `teleport_on_result` still
    /// armed would fly the camera somewhere on a reply that lands afterwards.
    pub fn close_suggestions(&mut self) {
        self.show_suggestions = false;
        self.teleport_on_result = false;
    }

    /// Put a query in the Search panel and open its result list, as if it had
    /// been typed. The debounce and the "has the text changed" gate both live
    /// behind private fields, so a scripted run (`--ui search`) cannot reach the
    /// search path without this — and it has no pointer to click the dock's disc
    /// with either, so asking for focus is also what opens the panel
    /// (`render_search_panel`).
    pub fn seed_search(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.focus_search = true;
        self.force_search = true;
        self.show_suggestions = true;
        // Not a teleport: the shot wants the list on screen, not a jump to its
        // top hit the moment the lookup lands.
        self.teleport_on_result = false;
    }

    /// Record a visited destination, browser-style: drop any forward history,
    /// then append (skipping an exact duplicate of the current spot).
    fn record_visit(&mut self, entry: NavEntry) {
        if !self.history.is_empty() {
            self.history.truncate(self.history_pos + 1);
            if self
                .history
                .last()
                .is_some_and(|e| e.lat == entry.lat && e.lng == entry.lng)
            {
                return; // same place — don't stack a duplicate
            }
        }
        self.history.push(entry);
        self.history_pos = self.history.len() - 1;
    }

    /// Whether Back / Forward have anywhere to go.
    pub fn can_go_back(&self) -> bool {
        self.history_pos > 0
    }
    pub fn can_go_forward(&self) -> bool {
        self.history_pos + 1 < self.history.len()
    }

    /// Move the history cursor one step and queue a teleport to that entry,
    /// flagged so it isn't recorded as a new visit.
    pub fn go_back(&mut self) {
        if self.can_go_back() {
            self.history_pos -= 1;
            self.queue_history_entry();
        }
    }
    pub fn go_forward(&mut self) {
        if self.can_go_forward() {
            self.history_pos += 1;
            self.queue_history_entry();
        }
    }
    fn queue_history_entry(&mut self) {
        let e = &self.history[self.history_pos];
        self.pending_teleport = Some(Teleport {
            lat: e.lat,
            lng: e.lng,
            altitude: Some(e.altitude),
            pose: Some(e.pose.clone()),
            silent: false,
        });
        self.from_history = true;
    }
}

/// A pending teleport target consumed by [`apply_teleport`].
#[derive(Clone, Default)]
pub struct Teleport {
    pub lat: f64,
    pub lng: f64,
    /// Free-camera altitude to arrive at; `None` keeps the current altitude.
    pub altitude: Option<f64>,
    /// Camera orientation to restore; `None` keeps the current camera.
    pub pose: Option<CameraPose>,
    /// Arrive without announcing it — no sound, no arrival animation.
    ///
    /// **Default is to announce**, because the common case is somebody choosing a
    /// destination and the uncommon one is the app moving the world under them.
    /// The silent sites are the ones where a whoosh would be a lie about who
    /// acted: the startup location restore, a scripted `--shot` capture, the
    /// globe-descent re-anchor (a camera move that happens to carry the avatar),
    /// and the device-fix jumps in `locate` / `driver`, which fire whenever the
    /// GPS reports a big enough step and are nobody's decision at all.
    pub silent: bool,
}

impl Teleport {
    /// A plain geo jump that keeps the current camera orientation.
    pub fn geo(lat: f64, lng: f64, altitude: Option<f64>) -> Self {
        Self {
            lat,
            lng,
            altitude,
            pose: None,
            silent: false,
        }
    }

    /// The same jump, arriving unannounced. See [`Self::silent`].
    #[must_use]
    pub fn silently(mut self) -> Self {
        self.silent = true;
        self
    }

    /// A jump to a point somebody sent into a chat: their spot, from their view.
    pub fn point(point: &cartopolis_core::point::Point) -> Self {
        Self {
            lat: point.latitude,
            lng: point.longitude,
            altitude: point.view.map(|v| v.altitude),
            pose: point.view.map(|v| CameraPose::free_look(v.pitch, v.yaw)),
            silent: false,
        }
    }

    /// A jump to a parsed location (pasted coordinates, or the `?place=` launch
    /// URL). Restores the view too when the fix carries look angles — that is
    /// what makes a copied place a shareable *view*, not just a spot.
    pub fn fix(fix: LocationFix) -> Self {
        Self {
            lat: fix.lat,
            lng: fix.lng,
            altitude: fix.altitude,
            pose: fix
                .look
                .map(|(pitch, yaw)| CameraPose::free_look(pitch, yaw)),
            silent: false,
        }
    }
}

/// One visited location in the browser-style Back/Forward history. Records the
/// resolved spot *and* the view, so revisiting it restores the exact framing.
#[derive(Clone)]
pub struct NavEntry {
    pub lat: f64,
    pub lng: f64,
    pub altitude: f64,
    pub pose: CameraPose,
    /// What the place was called when it was visited, when whoever queued the
    /// teleport knew ([`GeoNavState::pending_label`]). A recents row reads far
    /// better as "Vismarkt" than as five decimals, and the coordinates are the
    /// honest fallback for a jump that arrived without a name.
    pub label: Option<String>,
}

impl NavEntry {
    /// How this entry names itself in a list.
    fn title(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| format!("{:.5}, {:.5}", self.lat, self.lng))
    }
}

/// A camera orientation snapshot, persisted with saved locations and bookmarks
/// (see `user_store`) so a jump restores the exact view, not just the spot.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CameraPose {
    /// "free", "avatar", or "earth" (empty = unspecified: keep the current camera).
    pub mode: String,
    /// Free-camera look (pitch, yaw) — `CameraState::free_camera_rotation`.
    pub free_rotation: [f32; 2],
    /// Orbit-camera parameters (avatar mode).
    pub azimuth: f32,
    pub pitch: f32,
    pub distance: f32,
    /// Earth (orbit) mode parameters; the focus itself comes from the jump's
    /// lat/lng, so only the view angles and altitude are stored.
    #[serde(default)]
    pub orbit_distance: f64,
    #[serde(default)]
    pub orbit_yaw: f32,
    #[serde(default)]
    pub orbit_pitch: f32,
}

/// The persisted name of a camera mode — the write half of the `match` in
/// [`CameraPose::apply`], kept beside it so the two cannot drift.
#[must_use]
pub fn mode_name(mode: CameraMode) -> &'static str {
    match mode {
        CameraMode::Free => "free",
        CameraMode::Avatar => "avatar",
        CameraMode::Orbit => "earth",
    }
}

impl CameraPose {
    /// Capture the current camera orientation.
    pub fn capture(cs: &CameraState) -> Self {
        Self {
            mode: mode_name(cs.mode).to_string(),
            free_rotation: [cs.free_camera_rotation.x, cs.free_camera_rotation.y],
            azimuth: cs.azimuth,
            pitch: cs.pitch,
            distance: cs.distance,
            orbit_distance: cs.orbit_distance,
            orbit_yaw: cs.orbit_yaw,
            orbit_pitch: cs.orbit_pitch,
        }
    }

    /// A free-flying camera looking in a given direction, from the (pitch, yaw)
    /// degrees a copied place carries.
    ///
    /// Free mode whatever the copy was made in, because a copied place records a
    /// *picture*, not a control scheme: the angles come off the live camera
    /// transform, so a free camera put at that spot pointing that way reproduces
    /// the view even if it was framed from the orbit camera. Restoring the mode
    /// too would mean carrying its parameterisation (orbit focus, tier, heading)
    /// through a five-number string — that is what bookmarks are for.
    ///
    /// The orbit fields get `CameraState`'s defaults rather than zeroes: `apply`
    /// writes them unconditionally, and a zero follow distance would put the
    /// camera inside the avatar's head the next time the user pressed Tab.
    pub fn free_look(pitch_deg: f32, yaw_deg: f32) -> Self {
        Self {
            mode: "free".to_string(),
            free_rotation: [pitch_deg.to_radians(), yaw_deg.to_radians()],
            azimuth: 0.0,
            pitch: std::f32::consts::PI / 6.0,
            distance: 6.0,
            ..Default::default()
        }
    }

    /// Apply to the camera state (no-op if `mode` is empty, e.g. an old
    /// bookmark saved before poses existed — then the current view is kept).
    pub fn apply(&self, cs: &mut CameraState) {
        cs.mode = match self.mode.as_str() {
            "free" => CameraMode::Free,
            "avatar" => CameraMode::Avatar,
            "earth" => CameraMode::Orbit,
            _ => return,
        };
        // Clamp the pitch away from exactly ±90°: there the horizontal forward
        // vector is zero and `camera_controls`' `normalize()` turns the camera
        // frame to NaN — which a scripted shot reports as a black capture with
        // exit 0, since the interactive clamp only runs on *input* and a pose
        // written by the shot harness (whose manifest default is precisely
        // −90) or a saved bookmark never passes through it. Half a degree off
        // nadir is visually still nadir.
        let pitch_max = std::f32::consts::FRAC_PI_2 - 0.01;
        cs.free_camera_rotation = Vec2::new(
            self.free_rotation[0].clamp(-pitch_max, pitch_max),
            self.free_rotation[1],
        );
        cs.azimuth = self.azimuth;
        cs.pitch = self.pitch;
        cs.distance = self.distance;
        if matches!(cs.mode, CameraMode::Free) {
            // The same flag Orbit sets below, and it was missing: `camera_controls`'
            // Free arm re-derives `free_camera_rotation` from the camera transform
            // whenever `free_initialized` is false, so a pose applied on a run that
            // has never been in Fly mode had its pitch overwritten on the very next
            // frame by whatever the startup Earth camera was pointing at (nadir).
            // That is why `--shot --look` could never frame a horizon.
            cs.free_initialized = true;
        }
        if matches!(cs.mode, CameraMode::Orbit) {
            // Restore the saved view angles; the focus is set from the jump
            // target in `apply_teleport`. Guard against a zero distance from an
            // old pose saved before Earth mode existed.
            cs.orbit_distance = if self.orbit_distance > 0.0 {
                self.orbit_distance
            } else {
                1500.0
            };
            cs.orbit_distance_target = cs.orbit_distance;
            cs.orbit_yaw = self.orbit_yaw;
            cs.orbit_pitch = self.orbit_pitch;
            cs.orbit_initialized = true;
        }
    }
}

impl Default for GeoNavState {
    fn default() -> Self {
        Self {
            query: String::new(),
            suggestions: Vec::new(),
            show_suggestions: false,
            error: None,
            stale: false,
            empty_since: None,
            last_edit: None,
            last_sent: String::new(),
            last_sent_at: None,
            in_flight: 0,
            cache: Vec::new(),
            teleport_on_result: false,
            force_search: false,
            results: Arc::new(Mutex::new(Vec::new())),
            pending_teleport: None,
            pending_label: None,
            route_request: None,
            pending_card: None,
            history: Vec::new(),
            history_pos: 0,
            from_history: false,
            focus_search: false,
            search_has_focus: false,
        }
    }
}

// ─── Geo ↔ world mapping ─────────────────────────────────────────────────────

/// Lat/lng → world metres (x = east, z = south), in the same anchored Web
/// Mercator frame `map_stream` places its tiles in, so a teleport lands exactly
/// on the imagery for that location.
pub fn lat_lng_to_world(anchor: (f64, f64, u32), lat: f64, lng: f64) -> (f64, f64) {
    let (clat, clng, zoom) = anchor;
    let gs = tile_to_meters(zoom, clat);
    let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, zoom);
    let (fx, fy) = lat_lng_to_tile_frac(lat, lng, zoom);
    ((fx - fcx) * gs, (fy - fcy) * gs)
}

/// One anchor's Mercator constants, resolved once.
///
/// [`world_to_lat_lng`] is the whole of this for a single point, and for a single
/// point that is the right shape. For a *bulk* conversion it is the wrong one
/// twice over. Everything `new` computes here — the ground scale, the anchor's
/// fractional tile, the zoom's tile count — depends only on the anchor, and the
/// per-call version paid six transcendentals for them on every sample:
/// `tile_to_meters`'s `cos`/`powi` and `lat_lng_to_tile_frac`'s `tan`/`cos`/`ln`.
/// `terrain::bake` called it 65,536 times per map anchor, on the main thread.
///
/// The second saving is the one worth naming in a type: **Mercator is separable**.
/// Longitude is a function of `wx` alone and latitude of `wz` alone, so a grid of
/// `n × n` samples needs `n` longitudes and `n` latitudes, not `n²` of each — and
/// the expensive half, latitude's `sinh().atan()`, is the half that collapses.
/// Splitting the conversion into two methods is what makes that fact usable
/// instead of merely true.
#[derive(Clone, Copy, Debug)]
pub struct WorldFrame {
    /// Ground metres per tile at the anchor's latitude.
    gs: f64,
    /// The anchor's own fractional tile coordinate.
    fcx: f64,
    fcy: f64,
    /// Tiles across the world at this zoom.
    n: f64,
}

impl WorldFrame {
    #[must_use]
    pub fn new(anchor: (f64, f64, u32)) -> Self {
        let (clat, clng, zoom) = anchor;
        let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, zoom);
        Self {
            gs: tile_to_meters(zoom, clat),
            fcx,
            fcy,
            n: 2.0_f64.powi(zoom as i32),
        }
    }

    /// Longitude of a world easting. Independent of `wz`.
    #[must_use]
    pub fn lng_at(&self, wx: f64) -> f64 {
        (wx / self.gs + self.fcx) / self.n * 360.0 - 180.0
    }

    /// Latitude of a world northing. Independent of `wx`, and the expensive one.
    #[must_use]
    pub fn lat_at(&self, wz: f64) -> f64 {
        let fy = wz / self.gs + self.fcy;
        ((1.0 - 2.0 * fy / self.n) * std::f64::consts::PI)
            .sinh()
            .atan()
            .to_degrees()
    }
}

/// Inverse of [`lat_lng_to_world`].
///
/// Delegates to [`WorldFrame`] so there is one Mercator inverse in the tree rather
/// than a bulk copy that can drift from the single-point one. Converting more than
/// a handful of points — anything grid-shaped — should build the frame once and
/// call its two halves directly.
pub fn world_to_lat_lng(anchor: (f64, f64, u32), wx: f64, wz: f64) -> (f64, f64) {
    let frame = WorldFrame::new(anchor);
    (frame.lat_at(wz), frame.lng_at(wx))
}

/// Where the camera is on Earth — the single definition of that, shared by the
/// HUD readout and the persisted last location (`user_store`).
///
/// Below the space tier the anchored Mercator frame *is* the ground truth: the
/// streamed tiles are keyed with the same mapping, so `world_to_lat_lng` names
/// exactly the imagery under the camera. In space it is meaningless — the camera
/// is thousands of kilometres from the anchor and the flat frame projects that
/// offset onto an arbitrary point of the map — so the geographic sub-point is
/// used instead.
///
/// Autosave used to skip that split and store the flat-frame value: quitting
/// from orbit wrote a mid-ocean "last location" (plus a far-side-negative local
/// `y` as altitude), which the next startup faithfully restored and re-anchored
/// to — no tiles, no buildings, until the user jumped to a bookmark.
pub fn camera_geo(
    anchor: (f64, f64, u32),
    globe: &crate::systems::globe::GlobeState,
    cell: &GridCell,
    tf: &Transform,
) -> (f64, f64) {
    let edge = WORLD_CELL_EDGE as f64;
    let cam = DVec3::new(
        cell.x as f64 * edge + tf.translation.x as f64,
        cell.y as f64 * edge + tf.translation.y as f64,
        cell.z as f64 * edge + tf.translation.z as f64,
    );
    if globe.in_space(cell, tf) {
        let dir = (cam - globe.center_local.as_dvec3())
            .normalize_or_zero()
            .as_vec3();
        crate::systems::globe::sub_point(globe.anchor_lat, globe.anchor_lng, dir)
    } else {
        world_to_lat_lng(anchor, cam.x, cam.z)
    }
}

/// One coordinate component, with an optional hemisphere letter ("6.5665W").
fn coord(s: &str, pos: char, neg: char) -> Option<f64> {
    let s = s.trim();
    let (body, sign) = if let Some(b) = s.strip_suffix([pos, pos.to_ascii_lowercase()]) {
        (b, 1.0)
    } else if let Some(b) = s.strip_suffix([neg, neg.to_ascii_lowercase()]) {
        (b, -1.0)
    } else {
        (s, 1.0)
    };
    body.trim().parse::<f64>().ok().map(|v| v * sign)
}

/// Parse a pasted coordinate pair: "53.21940, 6.56650", "53.2194 6.5665",
/// "53.2194N, 6.5665E", "(-33.9, 18.4)". Returns (lat, lng) when both values
/// are in range.
pub fn parse_lat_lng(input: &str) -> Option<(f64, f64)> {
    let cleaned = input.trim().replace(['(', ')', '°'], " ");
    let parts: Vec<&str> = cleaned
        .split([',', ';'])
        .flat_map(|p| p.split_whitespace())
        .collect();
    if parts.len() != 2 {
        return None;
    }
    let lat = coord(parts[0], 'N', 'S')?;
    let lng = coord(parts[1], 'E', 'W')?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng)).then_some((lat, lng))
}

/// A parsed location: a spot, and as much of the view as the text carried.
/// Produced by [`parse_location`] from pasted text or the `?place=` launch URL.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LocationFix {
    pub lat: f64,
    pub lng: f64,
    /// Free-camera altitude to arrive at; `None` keeps the current one.
    pub altitude: Option<f64>,
    /// Camera look angles in degrees, (pitch, yaw) — the `look` convention of
    /// `docs/shots.toml`. `None` keeps the current orientation.
    pub look: Option<(f32, f32)>,
}

/// Altitude range a pasted/launch fix may ask for, in metres. The floor keeps a
/// "0" from burying the camera; the ceiling is well above the globe handoff.
const FIX_ALT_RANGE: std::ops::RangeInclusive<f64> = 1.0..=100_000.0;

/// Like [`parse_lat_lng`], but also accepts the trailing altitude and camera
/// look angles the HUD copy button writes:
/// "53.21940, 6.56650", "53.21940, 6.56650, 120m",
/// "53.21940, 6.56650, 120m, -12.5°, 45.0°".
///
/// The degree signs and the `m` are decoration — the string is meant to be
/// readable in a chat message and pasteable into the search bar or a `?place=`
/// URL, so every unit suffix is optional on the way back in.
pub fn parse_location(input: &str) -> Option<LocationFix> {
    let cleaned = input.trim().replace(['(', ')', '°'], " ");
    let parts: Vec<&str> = cleaned
        .split([',', ';'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    // A single segment is the whitespace-separated form ("53.2194N 6.5665E");
    // anything else puts lat and lng in the first two segments.
    let (lat, lng) = match parts.len() {
        0 => return None,
        1 => parse_lat_lng(parts[0])?,
        _ => {
            let lat = coord(parts[0], 'N', 'S')?;
            let lng = coord(parts[1], 'E', 'W')?;
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
                return None;
            }
            (lat, lng)
        }
    };
    let altitude = match parts.get(2) {
        Some(a) => Some(
            a.trim_end_matches(['m', 'M'])
                .trim()
                .parse::<f64>()
                .ok()?
                .clamp(*FIX_ALT_RANGE.start(), *FIX_ALT_RANGE.end()),
        ),
        None => None,
    };
    // Look angles only count as a pair — half of one is a malformed string, not
    // a fix with a pitch.
    let look = match (parts.get(3), parts.get(4)) {
        (Some(p), Some(y)) => Some((p.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?)),
        (Some(_), None) => return None,
        _ => None,
    };
    Some(LocationFix {
        lat,
        lng,
        altitude,
        look,
    })
}

/// Where this session should start, if the launch asked for a specific place.
///
/// Three ways in, all of them the same string in the end (`systems::place_link`):
///
/// - **A tapped link on Android** — `cartopolis://go?place=…`, or the verified
///   `https://cartopolis.org/go?place=…` App Link. First, because it is an
///   instruction for *this* launch in the most literal sense available: someone
///   just tapped it.
/// - **`?place=53.21940,6.56650,120m,-12.5,45.0`** in the web client's own URL.
/// - **`CARTO_PLACE`** in the environment on desktop (a system property on
///   Android, see [`crate::env_value`]).
///
/// Every one of them accepts a whole link as readily as a bare coordinate string,
/// so pasting a shared URL into `CARTO_PLACE` works and needs no explaining.
///
/// Takes precedence over the persisted last location (see
/// `user_store::restore_start_location`) — a link that names a place and then
/// drops you where you left off would be no link at all.
///
/// **Called once, at startup.** The Android arm cannot answer anything else: the
/// launch intent is frozen for the life of the process
/// (`platform::android::launch_url` has the why), so polling it would re-teleport
/// the camera to a link the user followed hours ago.
pub fn launch_place() -> Option<LocationFix> {
    #[cfg(target_os = "android")]
    if let Some(url) = crate::platform::android::launch_url() {
        match crate::systems::place_link::place_from_link(&url) {
            Some(fix) => {
                tracing::info!(%url, "starting at the place this launch's link names");
                return Some(fix);
            }
            // Not a failure worth refusing the launch over: the app can be started
            // by a link that names no place at all once it handles more than one.
            None => tracing::warn!(%url, "launch link names no place we can parse"),
        }
    }

    let raw = crate::env_value("CARTO_PLACE")?;
    let fix = crate::systems::place_link::place_from_link(&raw);
    if fix.is_none() {
        tracing::warn!(place = %raw, "ignoring unparseable launch place");
    }
    fix
}

// ─── Nominatim lookup (background thread) ────────────────────────────────────

/// Percent-encode a URL query component (RFC 3986 unreserved set kept).
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn spawn_geocode(query: String, results: Arc<Mutex<Vec<GeocodeReply>>>) {
    // Detached fetch on the IoTaskPool (ADR-033 seam): no OS thread, so it works
    // on the wasm build too. A queue rather than a single slot, because several
    // lookups may now be outstanding (see `MAX_IN_FLIGHT`) and the one that
    // finishes first must not overwrite an answer `drive_geocode` has not read.
    IoTaskPool::get()
        .spawn(async move {
            let url = format!(
                "{}/search?format=jsonv2&limit={}&q={}",
                nominatim_base_url(),
                MAX_SUGGESTIONS,
                percent_encode(&query)
            );
            let result = geocode_fetch(url).await;
            if let Err(ref e) = result {
                tracing::warn!(query = %query, error = %e, "geocoding lookup failed");
            }
            if let Ok(mut guard) = results.lock() {
                guard.push(GeocodeReply { query, result });
            }
        })
        .detach();
}

/// Fetch and parse a Nominatim `/search` response into suggestions.
pub(crate) async fn geocode_fetch(url: String) -> Result<Vec<GeoSuggestion>, String> {
    let body = http::fetch_string(http::Request::get(url)).await?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let items = json.as_array().ok_or("unexpected Nominatim response")?;
    Ok(items
        .iter()
        .filter_map(GeoSuggestion::from_nominatim)
        .collect())
}

// ─── How far away, and which way ─────────────────────────────────────────────

/// Great-circle metres between two points.
///
/// Haversine, not the equirectangular approximation `driver::metres_between`
/// uses: that one is measuring tens of metres between consecutive route vertices,
/// this one routinely measures thousands of kilometres (searching "Amsterdam"
/// from the Netherlands returns one in Missouri), where the flat-earth error is
/// hundreds of km.
pub fn great_circle_m(from: (f64, f64), to: (f64, f64)) -> f64 {
    const R: f64 = 6_371_008.8; // mean Earth radius
    let (lat1, lat2) = (from.0.to_radians(), to.0.to_radians());
    let dlat = lat2 - lat1;
    let dlng = (to.1 - from.1).to_radians();
    let a = (dlat * 0.5).sin().powi(2) + lat1.cos() * lat2.cos() * (dlng * 0.5).sin().powi(2);
    2.0 * R * a.sqrt().clamp(0.0, 1.0).asin()
}

/// Initial great-circle bearing from `from` to `to`, in degrees clockwise from north.
fn bearing_deg(from: (f64, f64), to: (f64, f64)) -> f64 {
    let (lat1, lat2) = (from.0.to_radians(), to.0.to_radians());
    let dlng = (to.1 - from.1).to_radians();
    let y = dlng.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlng.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// The 8-point compass name for a bearing.
///
/// A letter and not an arrow on purpose: this is read as part of a sentence
/// ("820 m NE"), where a rotating arrow would have to be drawn rather than
/// written, and eight of them in a list read as noise.
fn compass_point(deg: f64) -> &'static str {
    const POINTS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    POINTS[(((deg + 22.5) % 360.0) / 45.0) as usize % 8]
}

/// "820 m NE", "12.4 km S", "8 738 km W" — how far a hit is from where the
/// viewer is now, which is the single most useful thing to say about a list of
/// identically named places.
pub fn distance_label(from: (f64, f64), to: (f64, f64)) -> String {
    let m = great_circle_m(from, to);
    let dir = compass_point(bearing_deg(from, to));
    if m < 30.0 {
        return "here".to_string();
    }
    if m < 950.0 {
        return format!("{m:.0} m {dir}");
    }
    let km = m / 1000.0;
    if km < 10.0 {
        format!("{km:.1} km {dir}")
    } else {
        // Thin space every three digits: "8 738 km" is scannable, "8738 km" isn't.
        let whole = format!("{km:.0}");
        let mut grouped = String::with_capacity(whole.len() + 2);
        for (i, c) in whole.chars().enumerate() {
            if i > 0 && (whole.len() - i) % 3 == 0 {
                grouped.push('\u{202F}');
            }
            grouped.push(c);
        }
        format!("{grouped} km {dir}")
    }
}

/// What a suggestion row came back with: the row was clicked (go there), or its
/// ⚑ was (route there).
///
/// A struct rather than a bare `Response` because the row now carries a second
/// verb. It keeps [`clicked`](Self::clicked) as an inherent method so a row still
/// reads at the call site exactly as it did when it *was* a `Response`.
pub struct SuggestionPick {
    pub response: egui::Response,
    routed: bool,
}

impl SuggestionPick {
    /// The row itself was clicked — go to this place.
    #[must_use]
    pub fn clicked(&self) -> bool {
        self.response.clicked()
    }

    /// The row's ⚑ was clicked — make this place the destination instead.
    #[must_use]
    pub fn routed(&self) -> bool {
        self.routed
    }
}

/// One suggestion row, shared by the Search panel and the route planner's
/// endpoint fields so the two lists cannot drift apart.
///
/// Two lines: the name (plus what kind of place it is) leads, and the address
/// context and the distance from `viewer` sit under it in the dim tone. The whole
/// block is one click target — `UiBuilder::sense` puts a single response over the
/// laid-out content, which a `Button` could not do without collapsing both lines
/// into one string and losing the type hierarchy that makes the row scannable.
///
/// No ⚑: in the *route panel's* dropdown the row already is the endpoint setter,
/// so a "route here" beside it would be the same click twice. Use
/// [`destination_row`] wherever the row is only a place.
pub fn suggestion_row(
    ui: &mut egui::Ui,
    s: &GeoSuggestion,
    viewer: Option<(f64, f64)>,
    width: f32,
) -> SuggestionPick {
    suggestion_row_inner(ui, s, viewer, width, false, false)
}

/// [`suggestion_row`] plus a ⚑ that routes to the place instead of flying to it.
///
/// `stale` dims the row: the answers on screen are for an earlier query than the
/// one in the field (see `GeoNavState::apply_suggestions`). They are still real
/// places and still worth clicking, so they are dimmed rather than removed or
/// disabled.
pub fn destination_row(
    ui: &mut egui::Ui,
    s: &GeoSuggestion,
    viewer: Option<(f64, f64)>,
    width: f32,
    stale: bool,
) -> SuggestionPick {
    suggestion_row_inner(ui, s, viewer, width, true, stale)
}

fn suggestion_row_inner(
    ui: &mut egui::Ui,
    s: &GeoSuggestion,
    viewer: Option<(f64, f64)>,
    width: f32,
    route_action: bool,
    stale: bool,
) -> SuggestionPick {
    // Reserve the highlight's place in the paint list *before* the content, so the
    // fill lands behind the text rather than over it.
    let bg = ui.painter().add(egui::Shape::Noop);
    let inner = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 1.0;
        if stale {
            ui.set_opacity(0.55);
        }
        let mut routed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // The route button takes the right edge first and the name gets what
            // is genuinely left. A `truncate()` label asks for everything
            // available, so laid out the other way round it would push the button
            // clean off the row — the same rule the Places rows follow.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if route_action {
                    // The icon the Route panel itself wears, so the button and the
                    // panel it opens are recognisably one thing.
                    routed = theme::action_button(ui, RightPanel::Route.icon())
                        .on_hover_text("Route here — make this the destination")
                        .clicked();
                }
                let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
                ui.allocate_ui_with_layout(
                    size,
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&s.name).strong())
                                .truncate()
                                .selectable(false),
                        );
                        if let Some(kind) = &s.kind {
                            ui.add(
                                egui::Label::new(egui::RichText::new(kind).small().weak())
                                    .truncate()
                                    .selectable(false),
                            );
                        }
                    },
                );
            });
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if let Some(here) = viewer {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(distance_label(here, (s.lat, s.lng)))
                            .small()
                            .color(theme::ACCENT_TEXT),
                    )
                    .selectable(false),
                );
            }
            if !s.context.is_empty() {
                ui.add(
                    egui::Label::new(egui::RichText::new(&s.context).small().weak())
                        .truncate()
                        .selectable(false),
                );
            }
        });
        routed
    });
    let routed = inner.inner;
    let resp = inner.response;
    if resp.hovered() {
        let v = ui.visuals().widgets.hovered;
        ui.painter().set(
            bg,
            egui::epaint::RectShape::new(
                resp.rect.expand2(egui::vec2(4.0, 3.0)),
                v.corner_radius,
                v.weak_bg_fill,
                egui::Stroke::NONE,
                egui::StrokeKind::Inside,
            ),
        );
    }
    SuggestionPick {
        response: resp.on_hover_cursor(egui::CursorIcon::PointingHand),
        routed,
    }
}

/// Hand a queued destination to the route planner and bring its panel up.
///
/// Goes through `RouteState`'s own setters and not `Waypoint::set`, for the
/// reason those methods' docs give: a caller that sets the field itself silently
/// skips the invalidate, leaving the previous route still drawn under a new
/// destination.
///
/// Which setter depends on whether we have a name. A named request takes
/// [`RouteState::set_endpoint_named`], which skips the reverse lookup entirely —
/// we already hold the better name, the one the user just read on the row they
/// tapped, and the geocoder would spend a round trip to come back with
/// "Kerkstraat 12" where the row said "Albert Heijn". Only a request that
/// arrives unnamed falls back to [`RouteState::set_endpoint_at`] and its
/// coordinate label.
fn apply_route_request(nav: &mut GeoNavState, route: &mut RouteState, panel: &mut RightPanel) {
    let Some(req) = nav.route_request.take() else {
        return;
    };
    let label = req.label.trim();
    if label.is_empty() {
        route.set_endpoint_at(req.end, req.lat, req.lng);
    } else {
        route.set_endpoint_named(req.end, label, req.lat, req.lng);
    }
    // The start fills itself in: opening the panel runs `default_start`, which is
    // where "here" is defined. Same contract as the map's "Route to here".
    *panel = RightPanel::Route;
    tracing::info!(lat = req.lat, lng = req.lng, "routing to a picked place");
}

/// Poll finished lookups into the UI state and send the debounced next query.
///
/// It also spends any queued [`RouteRequest`], which is not geocoding — it is
/// here because this is the geo-nav system that runs unconditionally every frame,
/// and the three destination sources that queue one are all UI systems that
/// cannot take `RouteState` themselves (see [`RouteRequest`]). The panel opens on
/// the frame this runs, which may be the one after the tap; a panel appearing a
/// frame late is not observable, and a dedicated system would be.
pub fn drive_geocode(
    mut nav: ResMut<GeoNavState>,
    mut route: ResMut<RouteState>,
    mut right_panel: ResMut<RightPanel>,
) {
    if nav.route_request.is_some() {
        apply_route_request(&mut nav, &mut route, &mut right_panel);
    }

    // Harvest every finished lookup. Each is cached under the query it answered
    // (so retyping never asks twice), but only the one matching what is in the
    // field right now reaches the UI — the others are replies the user has
    // already typed past.
    let replies: Vec<GeocodeReply> = nav
        .results
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default();
    for reply in replies {
        nav.in_flight = nav.in_flight.saturating_sub(1);
        let current = reply.query == nav.query.trim();
        match reply.result {
            Ok(list) => {
                nav.remember(reply.query, &list);
                if current {
                    nav.apply_suggestions(list);
                }
            }
            // Failures are not cached: a retry after the network comes back
            // should actually retry.
            Err(e) if current => {
                nav.suggestions.clear();
                nav.error = Some(format!("Search failed: {e}"));
                nav.show_suggestions = true;
                nav.teleport_on_result = false;
            }
            Err(_) => {}
        }
    }

    let query = nav.query.trim().to_string();
    let debounced = nav.last_edit.is_some_and(|t| t.elapsed() >= DEBOUNCE);
    let throttled = nav
        .last_sent_at
        .is_some_and(|t| t.elapsed() < *MIN_REQUEST_INTERVAL);
    let fresh = query != nav.last_sent || nav.force_search;
    // `place_from_link`, not `parse_location`: a pasted cartopolis link is a place
    // this client can answer on its own, and sending it to Nominatim as a search
    // term would be a request that can only come back empty.
    let eligible = fresh
        && query.len() >= MIN_QUERY_LEN
        && crate::systems::place_link::place_from_link(&query).is_none();

    // A query we have already answered costs nothing, so it does not wait for
    // anything: not the debounce (which exists only to avoid firing a request
    // per keystroke) and not the rate limit (which exists only to be polite to
    // the endpoint). Backspacing through a query, or retyping one, updates on
    // the very next frame.
    if eligible {
        if let Some(list) = nav.cached(&query) {
            nav.force_search = false;
            nav.last_edit = None;
            nav.last_sent = query;
            nav.apply_suggestions(list);
            return;
        }
    }

    let wanted = eligible && (debounced || nav.force_search);
    if wanted && !throttled && nav.in_flight < MAX_IN_FLIGHT {
        nav.in_flight += 1;
        nav.force_search = false;
        nav.last_edit = None;
        nav.last_sent = query.clone();
        nav.last_sent_at = Some(Instant::now());
        spawn_geocode(query, nav.results.clone());
    } else if nav.force_search && (query.len() < MIN_QUERY_LEN || !fresh) {
        nav.force_search = false;
    }
}

// ─── The Search panel ────────────────────────────────────────────────────────

/// Back / Forward / Home — the location history, as a browser's toolbar row above
/// the field it belongs to.
///
/// **These were three named entries at the top of the app menu**, which put the
/// history two taps away from the address it is a history *of* and left the menu
/// carrying half a navigator. A back button belongs beside the thing you navigate
/// with, and here it is one row: three glyphs, no words, at the same rank as the
/// route panel's own icon actions.
///
/// Returns whether one of them fired, so the panel can drop a stale suggestion
/// list — the hits on screen answer a query about where you *were*.
fn history_row(
    ui: &mut egui::Ui,
    nav: &mut GeoNavState,
    home: Option<&crate::systems::user_store::SavedLocation>,
) -> bool {
    let mut acted = false;
    ui.horizontal(|ui| {
        if theme::action_enabled(ui, nav.can_go_back(), icons::BACK)
            .on_hover_text("Back to the previous location")
            .on_disabled_hover_text("No previous location")
            .clicked()
        {
            nav.go_back();
            acted = true;
        }
        if theme::action_enabled(ui, nav.can_go_forward(), icons::FORWARD)
            .on_hover_text("Forward to the next location")
            .on_disabled_hover_text("No next location")
            .clicked()
        {
            nav.go_forward();
            acted = true;
        }
        if theme::action_enabled(ui, home.is_some(), icons::HOME)
            .on_hover_text("Go to your Home location")
            .on_disabled_hover_text("No Home set — use Set Home here, below")
            .clicked()
        {
            if let Some(h) = home {
                nav.pending_teleport = Some(Teleport {
                    lat: h.lat,
                    lng: h.lng,
                    altitude: Some(h.altitude),
                    pose: Some(h.pose.clone()),
                    silent: false,
                });
                acted = true;
            }
        }
    });
    acted
}

/// What the two actions at the foot of the Search panel ask for.
///
/// They were in the app menu, one separator below the history, and they came here
/// with it: both name *this view* — the one the field above navigated to — and
/// neither has anything to do with the settings and tools the menu is otherwise
/// made of. `Copy link` is also the exact inverse of the field it now sits under,
/// since what it writes is what Search accepts back.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ViewAction {
    SetHome,
    CopyLink,
    /// Send this view into a private chat, as a point.
    SendToChat,
}

/// The foot of the panel: Set Home here, Copy link.
///
/// `done` is the action that fired within the last flash window, so the row that
/// fired can show a ✔ — these are the two verbs in the app whose whole result is
/// invisible (a preference written, a string on the clipboard), and both leave the
/// panel open.
fn view_actions_ui(
    ui: &mut egui::Ui,
    enabled: bool,
    can_send: bool,
    done: Option<ViewAction>,
) -> Option<ViewAction> {
    let mut fired = None;
    // Its own top-down block, because the caller draws this **first, in a
    // bottom-up layout**: the results list above is an `auto_shrink([false,
    // false])` `ScrollArea`, which takes every point of height that is left, so
    // anything laid out after it is off the bottom of the panel. Without the
    // block, a bottom-up parent would also stack these three in reverse.
    ui.vertical(|ui| {
        ui.add_space(4.0);
        ui.separator();
        ui.add_enabled_ui(enabled, |ui| {
            let home_icon = if done == Some(ViewAction::SetHome) {
                icons::CHECK
            } else {
                icons::HOME
            };
            if theme::menu_entry_enabled(ui, enabled, home_icon, "Set Home here")
                .on_hover_text("Home (above) returns to this spot and this view")
                .clicked()
            {
                fired = Some(ViewAction::SetHome);
            }
            let copy_icon = if done == Some(ViewAction::CopyLink) {
                icons::CHECK
            } else {
                icons::COPY
            };
            if theme::menu_entry_enabled(ui, enabled, copy_icon, "Copy link")
                .on_hover_text(
                    "Copy a link to this place — position and view. It opens the app on a \
                 phone that has it, the web client everywhere else, and pastes into the \
                 field above.",
                )
                .clicked()
            {
                fired = Some(ViewAction::CopyLink);
            }
            // Only with somebody to send it to: private chats need a session.
            if can_send
                && theme::menu_entry_enabled(ui, enabled, icons::CHAT, "Send this view to a chat")
                    .on_hover_text(
                        "Send this place and this view into a private chat. Whoever opens it \
                         sees it from exactly where you are looking.",
                    )
                    .clicked()
            {
                fired = Some(ViewAction::SendToChat);
            }
        });
        // The panel's own bottom edge is the screen's on a phone, and a row
        // flush against it reads as cut off.
        ui.add_space(6.0);
    });
    fired
}

/// Draw the search field and its results. `viewer` is where the camera is on
/// Earth, used to say how far away each hit is; `None` before the world anchor
/// exists, and the rows simply omit the distance.
///
/// **This was a floating card in the top-left corner and is a panel now**
/// (`RightPanel::Search`, opened from the dock's 🔍 disc), which changes two
/// things in the body below. The field takes the panel's width instead of a
/// fixed 215 pt, and the hits are laid out *inline* rather than in an
/// `Area` hung under the field: a dropdown over the map has to disappear the
/// moment focus leaves it or it covers the thing you are navigating, and a panel
/// has no such duty — the list stays until you pick from it, retype, or close the
/// panel. That matters most on the phone, where the on-screen keyboard docks over
/// the bottom half of the screen and a dropdown anchored to a corner card had
/// nowhere left to be.
pub fn search_ui(ui: &mut egui::Ui, nav: &mut GeoNavState, viewer: Option<(f64, f64)>) {
    let field = crate::systems::text_touch::field(
        ui,
        egui::TextEdit::singleline(&mut nav.query)
            .desired_width(ui.available_width())
            .margin(egui::Margin::symmetric(8, 6))
            .hint_text("Search place, or paste \"lat, lng\"…"),
    );
    // Ctrl+L from the top bar asks the field to grab focus this frame.
    if std::mem::take(&mut nav.focus_search) {
        field.request_focus();
    }
    // Report focus so the auto-hiding top bar stays up while the user types.
    nav.search_has_focus = field.has_focus();
    if nav.in_flight > 0 {
        theme::busy(ui, "Searching…");
    }

    if field.changed() {
        nav.last_edit = Some(Instant::now());
        nav.teleport_on_result = false;
        nav.error = None;
        nav.show_suggestions = true;
        // Re-arm the "No results" grace on every keystroke: the answer on screen
        // is for a query that is now one character old, so whatever it said about
        // emptiness says nothing about this one. Emptying the field entirely drops
        // the list, which is what puts the recents back.
        nav.empty_since = None;
        if nav.query.trim().is_empty() {
            nav.forget_suggestions();
        }
    }
    if field.gained_focus() {
        // Browser UX: select the whole query on focus so typing replaces it
        // (and Enter re-submits the shown address without clearing it first).
        let len = nav.query.chars().count();
        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), field.id) {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(len),
                )));
            state.store(ui.ctx(), field.id);
        }
        if !nav.suggestions.is_empty() {
            nav.show_suggestions = true;
        }
    }

    let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if submitted {
        if let Some(fix) = crate::systems::place_link::place_from_link(&nav.query) {
            nav.pending_teleport = Some(Teleport::fix(fix));
            nav.show_suggestions = false;
        } else if nav.query.trim() == nav.last_sent && !nav.stale && !nav.suggestions.is_empty() {
            // …but never off a *stale* list: those hits answer a query the field no
            // longer holds, and Enter is the one gesture that would fly there
            // without the user having read the row.
            let first = nav.suggestions[0].clone();
            nav.pending_teleport = Some(Teleport::geo(first.lat, first.lng, None));
            nav.pending_card = Some(card_place(&first));
            nav.pending_label = Some(first.name);
            nav.query = first.label;
            nav.show_suggestions = false;
        } else {
            // Search now; jump to the top hit when it lands.
            nav.force_search = true;
            nav.teleport_on_result = true;
        }
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        nav.show_suggestions = false;
    }

    // Suggestion dropdown under the field. Pasted coordinates — or a pasted
    // cartopolis link, which is the same thing wearing a URL — get a single direct
    // "Go to" entry instead of a lookup.
    let coord_fix = crate::systems::place_link::place_from_link(&nav.query);
    let coord_entry = coord_fix.map(|fix| {
        let mut s =
            GeoSuggestion::plain(format!("{:.5}, {:.5}", fix.lat, fix.lng), fix.lat, fix.lng);
        s.name = format!("Go to {:.5}, {:.5}", fix.lat, fix.lng);
        s.kind = Some("coordinates".to_string());
        if let Some(a) = fix.altitude {
            s.context = format!("at {a:.0} m");
        }
        if fix.look.is_some() {
            if !s.context.is_empty() {
                s.context.push_str(", ");
            }
            s.context.push_str("with the pasted view");
        }
        s
    });
    // With nothing typed, the panel offers where you have already been. That list
    // needs no keyboard, which is the whole point of it on a phone: Back walks the
    // history one step at a time and is the only other way to reach it. It no
    // longer waits for the field to hold focus — a panel opened and not typed into
    // is exactly when the recents are the whole of what it has to offer.
    let recents: Vec<(usize, GeoSuggestion)> = if nav.query.trim().is_empty() {
        nav.recent_entries()
            .map(|(i, e)| {
                let mut s = GeoSuggestion::plain(e.title(), e.lat, e.lng);
                s.kind = Some("recent".to_string());
                (i, s)
            })
            .collect()
    } else {
        Vec::new()
    };
    let status = nav.status();
    let stale = nav.stale;
    // Set by the error chip below; consumed after the closure, since `nav` is
    // borrowed for the duration of it.
    let mut dismiss_error = false;
    let has_content = coord_entry.is_some()
        || !nav.suggestions.is_empty()
        || !recents.is_empty()
        || status.is_some();
    // Shown while the field has something to answer, and whenever it is empty (a
    // panel just opened offers the recents). Picking a hit puts the place's name
    // in the field and clears `show_suggestions`, which is what takes the list
    // down — there is no focus rule doing it any more.
    if !has_content || !(nav.show_suggestions || nav.query.trim().is_empty()) {
        return;
    }

    let mut picked: Option<GeoSuggestion> = None;
    let mut routed: Option<GeoSuggestion> = None;
    let mut picked_recent: Option<usize> = None;
    ui.add_space(6.0);
    // The rows can outrun a phone's viewport, and a `Panel` clips rather than
    // scrolls — the same reason every other panel in this app wraps its body.
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let width = ui.available_width();
            let entries: Vec<&GeoSuggestion> =
                coord_entry.iter().chain(nav.suggestions.iter()).collect();
            for (i, s) in entries.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                // The coordinate entry is never stale — it is derived from what
                // is in the field this instant, not from an answer.
                let dim = stale && !(i == 0 && coord_entry.is_some());
                let row = destination_row(ui, s, viewer, width, dim);
                if row.clicked() {
                    picked = Some((*s).clone());
                }
                if row.routed() {
                    routed = Some((*s).clone());
                }
            }
            if !recents.is_empty() {
                ui.label(egui::RichText::new("Recent").small().weak());
                for (n, (idx, s)) in recents.iter().enumerate() {
                    if n > 0 {
                        ui.separator();
                    }
                    let row = destination_row(ui, s, viewer, width, false);
                    if row.clicked() {
                        picked_recent = Some(*idx);
                    }
                    if row.routed() {
                        routed = Some(s.clone());
                    }
                }
            }
            if let Some(msg) = &status {
                // Was `ui.weak()` — the quietest tone in the palette, on the one
                // string in this panel that most needs to be read. A geocoding
                // failure and "no results" arrive through the same slot, so the
                // chip only comes out for the failure.
                if nav.error.is_some() {
                    dismiss_error = theme::inline_error(ui, msg.as_str());
                } else {
                    ui.label(
                        egui::RichText::new(msg.as_str())
                            .size(theme::SIZE_SUB)
                            .color(theme::TEXT_DIM),
                    );
                }
            }
        });

    if dismiss_error {
        nav.error = None;
    }
    if let Some(s) = routed {
        // Routing to a hit does *not* fly there: the whole point is that you are
        // planning a journey to somewhere you are not.
        nav.route_to(s.name, s.lat, s.lng);
        nav.show_suggestions = false;
        nav.teleport_on_result = false;
    } else if let Some(i) = picked_recent {
        // A revisit restores the framing it was seen with, exactly as Back does —
        // but it is a fresh visit, not a history move, so it appends.
        let (tp, label, card) = {
            let e = &nav.history[i];
            (
                Teleport {
                    lat: e.lat,
                    lng: e.lng,
                    altitude: Some(e.altitude),
                    pose: Some(e.pose.clone()),
                    silent: false,
                },
                e.label.clone(),
                crate::systems::place_card::CardPlace {
                    name: e.title(),
                    kind: Some("recent".to_string()),
                    context: String::new(),
                    lat: e.lat,
                    lng: e.lng,
                },
            )
        };
        nav.pending_label = label;
        nav.pending_teleport = Some(tp);
        nav.pending_card = Some(card);
        nav.show_suggestions = false;
    } else if let Some(s) = picked {
        // The direct coordinate entry keeps its pasted altitude and view; a
        // Nominatim hit is only a spot. It also leaves the field alone: the user
        // typed those coordinates, and replacing them with "Go to 53.21940, …"
        // would make a re-submit unparseable.
        let fix = coord_fix.filter(|f| f.lat == s.lat && f.lng == s.lng);
        let mut card = card_place(&s);
        if fix.is_some() {
            // The row said "Go to 53.21940, …" — an imperative that reads wrong
            // as a card *title*; the bare coordinates are the place's name.
            card.name = s.label.clone();
        }
        nav.pending_card = Some(card);
        match fix {
            Some(fix) => nav.pending_teleport = Some(Teleport::fix(fix)),
            None => {
                nav.pending_teleport = Some(Teleport::geo(s.lat, s.lng, None));
                nav.pending_label = Some(s.name);
                nav.query = s.label;
            }
        }
        nav.show_suggestions = false;
        nav.teleport_on_result = false;
    }
}

/// A suggestion, reshaped for the place card: the same name, tag and address
/// context the row showed, so the card reads as the row that opened it.
fn card_place(s: &GeoSuggestion) -> crate::systems::place_card::CardPlace {
    crate::systems::place_card::CardPlace {
        name: s.name.clone(),
        kind: s.kind.clone(),
        context: s.context.clone(),
        lat: s.lat,
        lng: s.lng,
    }
}

/// The Search panel (dock ▸ 🔍, or `Ctrl+L`). Ordered in the panel `.chain()`
/// like every other panel — see the note there; an unordered panel loses egui's
/// hit test at random.
///
/// The shortcut is handled here rather than where the dock is drawn because it
/// has to work with the panel *closed*, and this is the system that owns the
/// panel: `Ctrl+L` is the browser's address-bar chord, and it both opens the
/// panel and hands the field the keyboard on the next frame
/// ([`GeoNavState::focus_search`], which is also how a scripted `--ui search` run
/// asks for it).
pub fn render_search_panel(
    // Spelled plainly rather than through its module path on purpose: the guard
    // that every root-`Ui` panel is in the panel chain
    // (`egui_manager::every_root_ui_panel_is_in_the_chain`) recognises a panel
    // system by the literal `ResMut<EguiManager>`, so a qualified path here would
    // quietly opt this panel out of the one check that catches an unordered one.
    mut egui: ResMut<EguiManager>,
    mut right_panel: ResMut<RightPanel>,
    mut nav: ResMut<GeoNavState>,
    keyboard: Res<ButtonInput<KeyCode>>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    map_anchor: Res<crate::resources::MapAnchor>,
    // Home and the two view actions came here from the app menu, and they bring
    // their two resources with them: the saved Home lives in `user.json`, and
    // Set Home records the camera pose alongside the coordinate.
    mut store: ResMut<crate::systems::user_store::UserStore>,
    camera_state: Res<CameraState>,
    // Which view action last fired, and when — the ✔ flash. A `Local`, like the
    // one the app menu kept for the same two entries.
    mut flash: Local<Option<(ViewAction, Instant)>>,
    // "Send this view to a chat" hands a point to the chat panel to place.
    mut chat: ResMut<crate::systems::chat::ChatState>,
    online: Option<Res<crate::resources::OnlineSession>>,
) {
    let ctrl = keyboard.pressed(KeyCode::ControlLeft) || keyboard.pressed(KeyCode::ControlRight);
    if ctrl && keyboard.just_pressed(KeyCode::KeyL) {
        *right_panel = RightPanel::Search;
        nav.focus_search = true;
    }
    // A scripted run seeds the query and asks for focus without a pointer to
    // click the disc with, so the request is enough to open the panel.
    if nav.focus_search {
        *right_panel = RightPanel::Search;
    }
    if !right_panel.is(RightPanel::Search) {
        // No field on screen → it can hold no focus, and nothing is listening for
        // the Enter that submits it.
        nav.search_has_focus = false;
        return;
    }

    // Where the camera is on Earth, for the hits' "how far away is this" line —
    // and, with the altitude beside it, for the two view actions at the foot.
    let cam = camera.single().ok();
    let viewer = cam
        .zip(map_anchor.geo)
        .map(|((cell, tf), anchor)| camera_geo(anchor, &globe, cell, tf));
    let place = viewer.zip(cam.map(|(cell, tf)| globe.altitude_m(cell, tf)));

    let done = flash
        .filter(|(_, at)| at.elapsed().as_secs_f32() < 1.5)
        .map(|(action, _)| action);
    let mut fired = None;
    let closed = crate::systems::egui_manager::right_panel(
        &mut egui,
        &mut right_panel,
        RightPanel::Search,
        "search_panel",
        320.0,
        |ui| {
            if history_row(ui, &mut nav, store.data.home.as_ref()) {
                nav.close_suggestions();
            }
            // The foot **reserves its space before the body is laid out**, which is
            // what a nested `TopBottomPanel` is for. The results list is an
            // `auto_shrink([false, false])` `ScrollArea` and takes every point of
            // height left to it, so two rows drawn after it are simply off the bottom
            // of the panel (they were), and a bottom-up layout around both puts them
            // over the top of it instead.
            egui::Panel::bottom("search_view_actions")
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show(ui, |ui| {
                    fired = view_actions_ui(ui, place.is_some(), online.is_some(), done);
                });
            search_ui(ui, &mut nav, viewer);
        },
    );
    match (fired, place) {
        (Some(ViewAction::SetHome), Some(((lat, lng), alt))) => {
            // Capture the current view so Home returns to it exactly.
            store.set_home(lat, lng, alt, CameraPose::capture(&camera_state));
            *flash = Some((ViewAction::SetHome, Instant::now()));
        }
        (Some(ViewAction::CopyLink), Some(((lat, lng), alt))) => {
            // Copy the view, not just the spot: the look angles come off the live
            // camera transform (as the dev panel's preset copy does), so they are
            // right in any camera mode, and following the link back reproduces
            // this exact framing.
            //
            // A *link* rather than the bare coordinate string it used to be,
            // because the string was only ever useful to someone who already knew
            // to paste it into this app's search bar — everyone else got five
            // numbers. The link carries the same five
            // (`place_link::place_param`) and the field above still accepts them
            // either way, so nothing that worked stopped working.
            let look = cam.map(|(_, tf)| {
                let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
                (pitch.to_degrees(), yaw.to_degrees())
            });
            crate::utils::clipboard::set_text(&crate::systems::place_link::web_link(
                lat, lng, alt, look,
            ));
            *flash = Some((ViewAction::CopyLink, Instant::now()));
        }
        (Some(ViewAction::SendToChat), Some(((lat, lng), alt))) => {
            // The same view the link would carry, as a point; the chat panel asks
            // which private chat it goes to. Named after where history says the
            // camera is, when it arrived by a named jump.
            let (pitch, yaw) = cam
                .map(|(_, tf)| {
                    let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
                    (pitch.to_degrees(), yaw.to_degrees())
                })
                .unwrap_or((-45.0, 0.0));
            let name = nav
                .history
                .get(nav.history_pos)
                .and_then(|e| e.label.clone());
            chat.point_draft = Some(cartopolis_core::point::Point {
                latitude: lat,
                longitude: lng,
                name,
                text: None,
                view: Some(cartopolis_core::point::PointView {
                    altitude: alt,
                    pitch,
                    yaw,
                }),
            });
            *right_panel = RightPanel::Chat;
        }
        _ => {}
    }
    if closed {
        nav.search_has_focus = false;
        nav.close_suggestions();
    }
}

/// Answer "send this view" from inside a chat (`chats`, the "+" menu): the view
/// the camera has now, as a point, into that chat's sealing queue. Here because
/// this is what knows where the camera is; the same point the Search panel's
/// "Send this view to a chat" builds.
pub fn answer_view_requests(
    mut chat: ResMut<crate::systems::chat::ChatState>,
    nav: Res<GeoNavState>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    if chat.view_requests.is_empty() {
        return;
    }
    let requests = std::mem::take(&mut chat.view_requests);
    let (Ok((cell, tf)), Some(anchor)) = (camera.single(), map_anchor.geo) else {
        chat.notice = Some(crate::systems::chat::ChatNotice {
            text: "There is no view to send yet.".into(),
            error: true,
        });
        return;
    };
    let (lat, lng) = camera_geo(anchor, &globe, cell, tf);
    let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
    let point = cartopolis_core::point::Point {
        latitude: lat,
        longitude: lng,
        name: nav
            .history
            .get(nav.history_pos)
            .and_then(|e| e.label.clone()),
        text: None,
        view: Some(cartopolis_core::point::PointView {
            altitude: globe.altitude_m(cell, tf),
            pitch: pitch.to_degrees(),
            yaw: yaw.to_degrees(),
        }),
    };
    for key in requests {
        if crate::systems::chat::ChatState::is_private(&key) {
            chat.private_points.push((key, point.clone()));
        }
    }
}

// ─── Globe-descent re-anchor ─────────────────────────────────────────────────

/// Turn a globe-descent re-anchor request (set by `camera_controls` when the
/// space camera drops onto a spot far from the current anchor) into a
/// re-anchoring teleport: arrive in Earth mode, near top-down, over the
/// sub-point, so detail streams there (ADR-025).
pub fn apply_camera_reanchor(mut camera_state: ResMut<CameraState>, mut nav: ResMut<GeoNavState>) {
    let Some((lat, lng, alt)) = camera_state.reanchor_request.take() else {
        return;
    };
    let pose = CameraPose {
        mode: "earth".to_string(),
        orbit_pitch: 1.4, // near top-down
        orbit_yaw: 0.0,
        orbit_distance: 0.0, // apply_teleport recomputes from the altitude
        ..Default::default()
    };
    nav.pending_teleport = Some(Teleport {
        lat,
        lng,
        altitude: Some(alt),
        pose: Some(pose),
        // A globe descent is a camera move that happens to bring the avatar; the
        // person flew here rather than jumping here.
        silent: true,
    });
}

// ─── Teleport ────────────────────────────────────────────────────────────────

/// Move the avatar and camera to a picked geo target. Runs once the world
/// anchor exists (the target stays pending until then). Positions are set as
/// big_space `GridCell` + local translation, the same convention the streamed
/// map uses, so tiles and buildings load around the destination automatically.
/// Online, the sim still owns the avatar, so the server may pull it back.
pub fn apply_teleport(
    mut nav: ResMut<GeoNavState>,
    mut stream: ResMut<MapStream>,
    mut camera_state: ResMut<CameraState>,
    mut avatar_state: ResMut<AvatarState>,
    globe: Res<crate::systems::globe::GlobeState>,
    online: Option<Res<crate::resources::OnlineSession>>,
    mut cameras: Query<(&mut GridCell, &mut Transform), (With<FreeCamera>, Without<Avatar>)>,
    mut avatars: Query<(&mut GridCell, &mut Transform), (With<Avatar>, Without<FreeCamera>)>,
) {
    if stream.geo_anchor().is_none() || nav.pending_teleport.is_none() {
        return; // nothing to do, or world not initialized yet (stay pending)
    }
    // A target saved in the space tier needs the globe to reconstruct its orbit
    // (radius, centre, north axis); firing before `globe.ready` would drop it to
    // the surface arm and strand it at 250 km. Hold it pending until the globe
    // exists — relevant for the startup restore of a location bookmarked from orbit.
    let space_target = nav.pending_teleport.as_ref().is_some_and(|tp| {
        tp.altitude
            .is_some_and(|a| a > crate::systems::globe::GLOBE_SHOW_ALT as f64)
    });
    if space_target && !globe.ready {
        return;
    }
    // The altitude we'll record in history when the target carries none (a plain
    // geo jump keeps the current altitude) — read before the camera moves.
    let current_alt = cameras
        .single()
        .ok()
        .map(|(cell, tf)| globe.altitude_m(cell, tf));
    let tp = nav.pending_teleport.take().unwrap();
    // Taken with the teleport it belongs to, never on its own: a label left
    // behind by a jump that never applied would end up naming the next one.
    let label = nav.pending_label.take();
    // Announce the arrival, unless the jump asked not to be (`Teleport::silent`).
    // Published here rather than at each call site because this is the frame the
    // avatar is actually *placed* on — a teleport queued before the world anchor
    // exists sits pending for as long as it takes, and a sound fired when it was
    // queued would play over an empty map minutes early.
    if !tp.silent {
        avatar_state.teleport_sequence = avatar_state.teleport_sequence.wrapping_add(1);
    }
    let (lat, lng, target_alt) = (tp.lat, tp.lng, tp.altitude);
    // Restore the saved camera orientation first, so the placement below and
    // camera_controls (which runs after) use the intended mode and angle.
    if let Some(pose) = &tp.pose {
        pose.apply(&mut camera_state);
    }

    // Re-anchor the map/globe/buildings to the destination so detail streams
    // there and the camera stays near the origin, where the flat frame is exact
    // (the anchor-less model — no fixed region). The anchor snaps to the
    // destination's tile centre (the convention all placement math assumes), so
    // the arrival point is the destination's small offset within that frame.
    // **A short jump keeps the frame.** A re-anchor rebuilds the world — every
    // ground and building cell, the terrain field, the stores — and a jump across
    // a city lands on ground most of which is already built. Within
    // `SHORT_JUMP_M` of the anchor the camera and the avatar are simply moved in
    // the frame they are in (step 3 of `level-of-detail-tile-tree.md`); past it the
    // flat near field and the curved basemap disagree by more than a couple of
    // metres at the ring's edge, and the frame is re-centred as before.
    let current = stream.geo_anchor();
    let short = !space_target
        && current.is_some_and(|a| {
            let (dx, dz) = lat_lng_to_world(a, lat, lng);
            dx.hypot(dz) < SHORT_JUMP_M
        });
    let (wx, wz) = match current {
        Some(a) if short => lat_lng_to_world(a, lat, lng),
        _ => {
            stream.request_reanchor(lat, lng);
            let base_zoom = current
                .map(|(_, _, z)| z)
                .unwrap_or(cartopolis_core::world::DEFAULT_TILE_ZOOM);
            let snapped = crate::systems::map_stream::snap_anchor(lat, lng, base_zoom);
            lat_lng_to_world((snapped.0, snapped.1, base_zoom), lat, lng)
        }
    };
    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);

    let (avatar_cell, avatar_local) = grid.translation_to_grid(DVec3::new(wx, AVATAR_GROUND_Y, wz));
    // The entity and the state move together or not at all. They used to be
    // written a line apart with only the *entity* guarded, so on any frame the
    // query missed, `AvatarState` claimed a position the avatar was not at — and
    // the follow camera frames `display_position` while the model is drawn at the
    // transform, so the two disagreeing is precisely "the camera looks at empty
    // ground". The query needs a `GridCell`, which `main::attach_avatar_to_bigspace`
    // inserts through `Commands`, so the frame it misses is a real one: the first,
    // which is the frame `restore_start_location` teleports on.
    //
    // `apply_teleport` is ordered after that attach now, so the miss should not
    // happen at all — this guard is the belt to that braces, and it is cheap.
    // Skipping rather than half-applying is the right failure: an avatar that
    // stays put with state that agrees is recoverable by the next teleport, while
    // state that lies about where the avatar is never resolves on its own.
    if let Ok((mut cell, mut tf)) = avatars.single_mut() {
        place_avatar(
            &mut cell,
            &mut tf,
            &mut avatar_state,
            online.as_deref(),
            avatar_cell,
            avatar_local,
        );
    } else {
        tracing::warn!(
            "geo_nav: teleported the camera but not the avatar — no avatar with a \
             GridCell yet; avatar state left where the entity actually is"
        );
    }

    if let Ok((mut cell, mut tf)) = cameras.single_mut() {
        match camera_state.mode {
            // Same cell + orbit offset as camera_controls computes, so the
            // follow lerp starts already converged instead of flying across
            // the world.
            CameraMode::Avatar => {
                let horizontal = camera_state.distance * camera_state.pitch.cos();
                let offset = Vec3::new(
                    camera_state.azimuth.sin() * horizontal,
                    camera_state.distance * camera_state.pitch.sin() + 1.5,
                    camera_state.azimuth.cos() * horizontal,
                );
                *cell = avatar_cell;
                tf.translation = avatar_local + offset;
                tf.look_at(avatar_local + Vec3::Y * 1.5, Vec3::Y);
            }
            // Use the target's altitude when it carries one (copied locations,
            // bookmarks, the startup restore); otherwise keep the current
            // altitude (with a floor) over the new spot.
            //
            // The two cases take **different floors, and that is the point**.
            // `MIN_TELEPORT_ALTITUDE` exists so a jump we chose the altitude for
            // arrives somewhere the destination is visible — it is a default, and
            // a default has no business overriding a number the caller stated.
            // Applying it to `target_alt` silently rewrote every explicit
            // altitude below it: a bookmark saved at 30 m reopened at 60, a
            // `?place=…,25m,…` link landed at 60, and `--shot --at lat,lng,35`
            // photographed 60 m while `ShotMetrics` dutifully reported the 60 it
            // had been moved to. That last one is why this was found — a harness
            // built to answer "what does it look like from there?" could not be
            // aimed below the floor, and said nothing about refusing.
            //
            // An explicit altitude still gets `MIN_CAMERA_ALTITUDE`, which is a
            // different kind of limit: not "far enough to see" but "not inside
            // the ground". Terrain relief is added to the camera downstream, so
            // this is metres above the anchor's ground rather than above the
            // local surface, and it is deliberately clear of head height.
            CameraMode::Free => {
                let altitude = free_teleport_altitude(target_alt, || {
                    cell.y as f64 * WORLD_CELL_EDGE as f64 + tf.translation.y as f64
                });
                let (new_cell, local) = grid.translation_to_grid(DVec3::new(wx, altitude, wz));
                *cell = new_cell;
                tf.translation = local;
                // Land at rest: fly inertia carried across a teleport would slide
                // the camera off the place you jumped to.
                camera_state.free_velocity = Vec3::ZERO;
            }
            // Earth mode with a bookmark/copied location saved from the space
            // tier: return to space over the target instead of the surface. The
            // surface arm below caps the orbit at 250 km (ORBIT_MAX_DISTANCE_SURFACE)
            // and forces `orbit_space = false`, which is right for a ground jump
            // but would strand a "bookmarked from orbit" view just above the
            // atmosphere. Space uses a different parameterisation (`orbit_dir`
            // from Earth's centre, not a ground focus + pitch/yaw), so we set it
            // up directly — the same recipe as the `--shot` harness's
            // `place_space_orbit`. We re-anchor to `lat,lng` above, so straight up
            // (`+Y`) over the anchor *is* the direction to the target; heading 0
            // brings the globe up north-up. camera_controls repositions from these.
            CameraMode::Orbit
                if globe.ready
                    && target_alt
                        .is_some_and(|a| a > crate::systems::globe::GLOBE_SHOW_ALT as f64) =>
            {
                let alt = target_alt.unwrap();
                let center = globe.center_local.as_dvec3();
                let dir = DVec3::Y;
                camera_state.orbit_space = true;
                camera_state.orbit_initialized = true;
                camera_state.orbit_dir = dir;
                camera_state.orbit_distance = globe.radius as f64 + alt;
                camera_state.orbit_distance_target = camera_state.orbit_distance;
                camera_state.orbit_heading = 0.0;
                let north = globe.north_axis_local().as_dvec3();
                let up = crate::systems::free_camera::globe_up(dir, north, 0.0, north);
                camera_state.orbit_up = up;

                let cam_world = center + dir * camera_state.orbit_distance;
                let (new_cell, local) = grid.translation_to_grid(cam_world);
                *cell = new_cell;
                tf.translation = local;
                let cell_origin =
                    DVec3::new(new_cell.x as f64, new_cell.y as f64, new_cell.z as f64)
                        * WORLD_CELL_EDGE as f64;
                let pivot_local = (center - cell_origin).as_vec3();
                tf.look_at(pivot_local, up.as_vec3());
            }
            // Earth mode: recenter the orbit on the target. The view angles are
            // already restored (pose) or kept; a carried altitude sets how far
            // back to sit. camera_controls repositions from these next.
            CameraMode::Orbit => {
                // Always arrive in the surface (ground-focus) tier — a jump made
                // from space would otherwise carry an orbit-radius distance.
                camera_state.orbit_space = false;
                camera_state.orbit_focus = DVec3::new(wx, 0.0, wz);
                camera_state.orbit_pitch = camera_state.orbit_pitch.clamp(0.12, 1.55);
                if let Some(alt) = target_alt {
                    // distance from the focus that puts the camera at `alt` given
                    // the current tilt (altitude = distance · sin(pitch)).
                    let sp = (camera_state.orbit_pitch.sin() as f64).max(0.05);
                    camera_state.orbit_distance = (alt / sp).clamp(15.0, 250_000.0);
                } else {
                    // No altitude given: keep a close regional framing.
                    camera_state.orbit_distance = camera_state.orbit_distance.clamp(15.0, 5_000.0);
                }
                camera_state.orbit_distance_target = camera_state.orbit_distance;
                camera_state.orbit_initialized = true;

                let pitch = camera_state.orbit_pitch;
                let yaw = camera_state.orbit_yaw;
                let (cp, sp) = (pitch.cos() as f64, pitch.sin() as f64);
                let (sy, cy) = (yaw.sin() as f64, yaw.cos() as f64);
                let offset = DVec3::new(sy * cp, sp, cy * cp) * camera_state.orbit_distance;
                let cam_world = camera_state.orbit_focus + offset;
                let (new_cell, local) = grid.translation_to_grid(cam_world);
                *cell = new_cell;
                tf.translation = local;
                let cell_origin =
                    DVec3::new(new_cell.x as f64, new_cell.y as f64, new_cell.z as f64)
                        * WORLD_CELL_EDGE as f64;
                let focus_local = (camera_state.orbit_focus - cell_origin).as_vec3();
                tf.look_at(focus_local, Vec3::Y);
            }
        }
    }

    // Record the visit in the Back/Forward history — unless this teleport *was*
    // a Back/Forward move (the cursor already points at it). The pose is
    // captured after the arms above have settled the camera, so it restores the
    // exact framing on a later revisit.
    if !std::mem::take(&mut nav.from_history) {
        let altitude = target_alt.or(current_alt).unwrap_or(MIN_TELEPORT_ALTITUDE);
        let pose = CameraPose::capture(&camera_state);
        nav.record_visit(NavEntry {
            lat,
            lng,
            altitude,
            pose,
            label,
        });
    }

    tracing::info!(lat, lng, "teleported to geo target");
}

/// Put the local avatar at `cell` + `local` and, online, ask the sim to put it
/// there too. The avatar half of [`apply_teleport`], shared with
/// `session::keep_avatar_across_reconnect`, which has to place the avatar without
/// moving the camera.
pub fn place_avatar(
    cell: &mut GridCell,
    tf: &mut Transform,
    avatar_state: &mut AvatarState,
    online: Option<&crate::resources::OnlineSession>,
    avatar_cell: GridCell,
    avatar_local: Vec3,
) {
    *cell = avatar_cell;
    tf.translation = avatar_local;
    avatar_state.position = avatar_local;
    avatar_state.display_position = avatar_local;
    // A jump ends a sit, immediately and without the slide: the bench that was
    // being sat on belongs to the cells this teleport is about to retire, so
    // easing off it would ease off a seat that is no longer anywhere
    // (`resources::Seated` holds an offset, and after this the offset points at
    // a place in the new city).
    avatar_state.seated = None;
    // Online, the sim owns this position, so ask it to place the avatar too
    // (`NetMessage::Teleport`, protocol 17) — without this the next snapshot
    // pulled the body straight back to where it was, which is what every
    // online jump silently did before. The hold keeps the snapshots already
    // in flight (still carrying the old position) from yanking the avatar
    // back while the sim's confirmation is on its way
    // (`network::adopt_local_pose`).
    if let Some(sess) = online {
        sess.send(
            "teleport",
            cartopolis_core::NetMessage::Teleport {
                request_id: 0,
                position: avatar_local,
            },
        );
        avatar_state.teleport_hold = Some(web_time::Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::globe::{sub_point, GlobeState};

    const ANCHOR: (f64, f64, u32) = (53.2194, 6.5665, 16);

    fn suggestion(label: &str) -> GeoSuggestion {
        GeoSuggestion::plain(label, 53.0, 6.0)
    }

    /// A teleport that had to pick an altitude arrives high enough to see where
    /// it landed — that is what [`MIN_TELEPORT_ALTITUDE`] is for.
    #[test]
    fn a_derived_teleport_altitude_keeps_the_destination_in_view() {
        assert_eq!(
            free_teleport_altitude(None, || 5.0),
            MIN_TELEPORT_ALTITUDE,
            "a jump from near the ground must still show the destination"
        );
        assert_eq!(
            free_teleport_altitude(None, || 4_000.0),
            4_000.0,
            "and an altitude already above the floor is kept, not lowered"
        );
    }

    /// …but a **stated** altitude is an instruction, not a suggestion. This is
    /// the one that regressed: the floor was applied to both, so a bookmark at
    /// 30 m, a `?place=…,25m,…` link and `--shot --at lat,lng,35` all silently
    /// became 60 m — and the shot harness then reported the 60 as if it had been
    /// asked for, which is how it went unnoticed.
    #[test]
    fn a_stated_teleport_altitude_is_honoured_below_the_view_floor() {
        for stated in [35.0, 30.0, 25.0, MIN_CAMERA_ALTITUDE] {
            assert_eq!(
                free_teleport_altitude(Some(stated), || 1_000.0),
                stated,
                "an explicitly requested {stated} m must not be raised"
            );
        }
    }

    /// The floor a stated altitude *does* take is the one that keeps the near
    /// plane out of the ground — a different limit with a different job.
    #[test]
    fn a_stated_teleport_altitude_still_clears_the_ground() {
        assert_eq!(
            free_teleport_altitude(Some(0.0), || 1_000.0),
            MIN_CAMERA_ALTITUDE
        );
        assert_eq!(
            free_teleport_altitude(Some(-500.0), || 1_000.0),
            MIN_CAMERA_ALTITUDE
        );
        assert!(
            MIN_CAMERA_ALTITUDE < MIN_TELEPORT_ALTITUDE,
            "the keep-out-of-the-ground floor must stay under the see-it floor, \
             or stating an altitude gains nothing"
        );
    }

    /// The suggestion rows show a name, a kind and an address context, and all
    /// three come out of one flattened `display_name` plus two sibling fields.
    #[test]
    fn a_nominatim_hit_splits_into_name_kind_and_context() {
        let hit = serde_json::json!({
            "display_name": "Amsterdam, Noord-Holland, Nederland",
            "name": "Amsterdam",
            "addresstype": "city",
            "lat": "52.3730796",
            "lon": "4.8924534",
        });
        let s = GeoSuggestion::from_nominatim(&hit).expect("well-formed hit");
        assert_eq!(s.name, "Amsterdam");
        assert_eq!(s.context, "Noord-Holland, Nederland");
        assert_eq!(s.kind.as_deref(), Some("city"));
        // The full line survives — it is what a picked suggestion leaves behind
        // in the search field.
        assert_eq!(s.label, "Amsterdam, Noord-Holland, Nederland");
    }

    /// Plenty of hits carry no `name` at all (an unnamed way, a house number).
    /// Splitting the display line at its first comma is the same convention.
    #[test]
    fn a_hit_without_a_name_falls_back_to_the_first_segment() {
        let hit = serde_json::json!({
            "display_name": "12, Oosterstraat, Groningen, Nederland",
            "addresstype": "house_number",
            "lat": "53.2194",
            "lon": "6.5665",
        });
        let s = GeoSuggestion::from_nominatim(&hit).expect("well-formed hit");
        assert_eq!(s.name, "12");
        assert_eq!(s.context, "Oosterstraat, Groningen, Nederland");
        // Underscores are a database detail, not a word.
        assert_eq!(s.kind.as_deref(), Some("house number"));
    }

    /// The distance is the whole reason the rows got richer: it is what tells the
    /// Amsterdam next door from the one in Missouri.
    #[test]
    fn distance_scales_its_unit_and_names_the_direction() {
        let groningen = (53.2194, 6.5665);
        // Same spot: no useful direction to give.
        assert_eq!(distance_label(groningen, groningen), "here");
        // A few hundred metres north stays in metres.
        let near = (53.2224, 6.5665);
        assert!(
            distance_label(groningen, near).ends_with(" m N"),
            "got {}",
            distance_label(groningen, near)
        );
        // Amsterdam is ~147 km west-south-west; the thousands group with a thin
        // space only once there are four digits, so this one has none.
        let amsterdam = (52.3731, 4.8925);
        assert_eq!(distance_label(groningen, amsterdam), "147 km SW");
        // Amsterdam, Missouri — the hit this whole line exists to disambiguate.
        // North-west, not west: the bearing is the great-circle one, which leaves
        // the Netherlands over the pole side rather than along the parallel.
        let missouri = (39.4028, -93.9666);
        let far = distance_label(groningen, missouri);
        assert!(far.ends_with(" km NW"), "got {far}");
        assert!(far.contains('\u{202F}'), "four digits should group: {far}");
    }

    /// Haversine, not the flat approximation the driver uses: a quarter of the
    /// way round the planet is where the two disagree by thousands of km.
    #[test]
    fn great_circle_matches_a_known_quarter_meridian() {
        let quarter = great_circle_m((0.0, 0.0), (90.0, 0.0));
        assert!((quarter - 10_007_543.0).abs() < 1_000.0, "got {quarter}");
    }

    #[test]
    fn cache_answers_a_repeated_query_without_a_lookup() {
        let mut nav = GeoNavState::default();
        assert!(nav.cached("Groningen").is_none());

        nav.remember("Groningen".into(), &[suggestion("Groningen, Nederland")]);
        let hit = nav.cached("Groningen").expect("just remembered");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].label, "Groningen, Nederland");

        // Backspacing to a prefix we asked about earlier is a hit too — that is
        // the case the debounce cannot help with.
        nav.remember("Groninge".into(), &[suggestion("Groningen, Nederland")]);
        assert!(nav.cached("Groninge").is_some());
        // An empty answer is still an answer; re-asking would just return [].
        nav.remember("Gron".into(), &[]);
        assert_eq!(nav.cached("Gron").map(|l| l.len()), Some(0));
    }

    #[test]
    fn cache_evicts_oldest_first_and_refreshes_on_reuse() {
        let mut nav = GeoNavState::default();
        for i in 0..CACHE_CAP {
            nav.remember(format!("q{i}"), &[suggestion("x")]);
        }
        assert_eq!(nav.cache.len(), CACHE_CAP);

        // Touching the oldest entry moves it to the newest end, so the *next*
        // eviction takes q1 rather than the entry we just used.
        assert!(nav.cached("q0").is_some());
        nav.remember("q0".into(), &[suggestion("x")]);
        nav.remember("fresh".into(), &[suggestion("x")]);

        assert_eq!(nav.cache.len(), CACHE_CAP);
        assert!(nav.cached("q0").is_some(), "reused entry must survive");
        assert!(nav.cached("q1").is_none(), "oldest must be evicted");
        assert!(nav.cached("fresh").is_some());
    }

    /// The mitigation `docs/notes/geocoding-latency.md` asks for: Nominatim does
    /// no prefix matching, so a half-typed name answers `[]` as a matter of
    /// course. Clearing the list on that made the dropdown blink empty between
    /// keystrokes and announce "No results" for a query nobody had finished
    /// typing — the search read as broken exactly while it was working.
    #[test]
    fn an_empty_answer_keeps_the_last_list_and_holds_its_verdict() {
        let mut nav = GeoNavState::default();
        nav.apply_suggestions(vec![suggestion("Groningen, Nederland")]);
        assert!(!nav.stale);
        assert_eq!(nav.status(), None);

        // "Groninge" → "Groningen…" mid-word: nothing comes back.
        nav.apply_suggestions(Vec::new());
        assert_eq!(nav.suggestions.len(), 1, "the last real list must stay up");
        assert!(nav.stale, "…but marked, so the rows can be dimmed");
        assert_eq!(nav.status(), None, "and no verdict yet");

        // Once the empty answer has stood for the grace period, say so — under the
        // rows, which are still real places worth clicking.
        nav.empty_since = Some(
            Instant::now()
                .checked_sub(NO_RESULTS_GRACE * 2)
                .expect("the monotonic clock is older than the grace period"),
        );
        assert_eq!(nav.status().as_deref(), Some("No results"));

        // A real answer clears both.
        nav.apply_suggestions(vec![suggestion("Groningen, Nederland")]);
        assert!(!nav.stale);
        assert_eq!(nav.status(), None);
    }

    /// A failure is not a "no results", and does not wait: the network being down
    /// is a fact about now, not a verdict on the query.
    #[test]
    fn a_failure_is_reported_at_once() {
        let mut nav = GeoNavState::default();
        nav.error = Some("Search failed: timeout".to_string());
        assert_eq!(nav.status().as_deref(), Some("Search failed: timeout"));
    }

    /// Emptying the field drops the answers with it — a list of places under a
    /// blank query is not "the last thing you searched", it is a list with nothing
    /// tying it to what the bar says.
    #[test]
    fn emptying_the_field_forgets_the_answers() {
        let mut nav = GeoNavState::default();
        nav.apply_suggestions(vec![suggestion("Groningen, Nederland")]);
        nav.apply_suggestions(Vec::new());
        assert!(nav.stale);
        nav.forget_suggestions();
        assert!(nav.suggestions.is_empty() && !nav.stale && nav.empty_since.is_none());
    }

    /// "Route here" from a row is the sentence this whole path exists for, and it
    /// must behave exactly as the map's own "Route to here" does — including
    /// dropping the route that was drawn for the previous destination.
    #[test]
    fn routing_to_a_place_sets_the_destination_and_opens_the_panel() {
        // `set_endpoint_at` fires the reverse geocode on the `IoTaskPool`.
        bevy::tasks::IoTaskPool::get_or_init(Default::default);

        let mut nav = GeoNavState::default();
        let mut route = RouteState::default();
        let mut panel = RightPanel::None;

        nav.route_to("Vismarkt", 53.2194, 6.5665);
        apply_route_request(&mut nav, &mut route, &mut panel);

        assert!(nav.route_request.is_none(), "the request is spent");
        assert_eq!(route.to.coord, Some((53.2194, 6.5665)));
        // The name the user just read on the row, not the coordinates the reverse
        // lookup is only now going out to fetch.
        assert_eq!(route.to.label, "Vismarkt");
        assert!(panel.is(RightPanel::Route), "and the planner is on screen");

        // Routing somewhere is not flying there.
        assert!(nav.pending_teleport.is_none());
    }

    /// A nameless place still routes; it just wears its coordinates, which is what
    /// `set_endpoint_at` puts there for a map pick.
    #[test]
    fn routing_without_a_name_keeps_the_coordinate_label() {
        bevy::tasks::IoTaskPool::get_or_init(Default::default);
        let mut nav = GeoNavState::default();
        let mut route = RouteState::default();
        let mut panel = RightPanel::None;

        nav.route_to("  ", 53.0, 6.0);
        apply_route_request(&mut nav, &mut route, &mut panel);
        assert!(
            !route.to.label.is_empty() && !route.to.label.contains('📍'),
            "an unnamed pick keeps the coordinate label, and that label must not \
             carry a glyph the font cannot draw"
        );
    }

    /// The recents list: newest first, without the place we are standing on (that
    /// one is not somewhere to go), capped so the dropdown stays a dropdown.
    #[test]
    fn recents_read_newest_first_and_skip_where_we_are() {
        let mut nav = GeoNavState::default();
        for i in 0..(RECENT_SUGGESTIONS + 3) {
            nav.record_visit(NavEntry {
                lat: i as f64,
                lng: 0.0,
                altitude: 100.0,
                pose: CameraPose::default(),
                label: Some(format!("place {i}")),
            });
        }
        let names: Vec<String> = nav.recent_entries().map(|(_, e)| e.title()).collect();
        assert_eq!(names.len(), RECENT_SUGGESTIONS);
        let here = nav.history[nav.history_pos].title();
        assert!(
            !names.contains(&here),
            "where we already are is not a destination"
        );
        assert_eq!(names[0], format!("place {}", RECENT_SUGGESTIONS + 1));

        // A visit that arrived without a name still lists — as its coordinates,
        // which is all a startup restore can honestly say about itself.
        nav.record_visit(NavEntry {
            lat: 53.2194,
            lng: 6.5665,
            altitude: 100.0,
            pose: CameraPose::default(),
            label: None,
        });
        nav.record_visit(NavEntry {
            lat: 1.0,
            lng: 1.0,
            altitude: 100.0,
            pose: CameraPose::default(),
            label: None,
        });
        let names: Vec<String> = nav.recent_entries().map(|(_, e)| e.title()).collect();
        assert_eq!(names[0], "53.21940, 6.56650");
    }

    /// Enter must not fly to the top of a *stale* list: those hits answer a query
    /// the field no longer holds, and Enter is the one gesture that acts without
    /// the user having read the row.
    #[test]
    fn an_empty_answer_spends_a_pending_enter_without_jumping() {
        let mut nav = GeoNavState::default();
        nav.apply_suggestions(vec![suggestion("Groningen, Nederland")]);
        nav.teleport_on_result = true;
        nav.apply_suggestions(Vec::new());
        assert!(!nav.teleport_on_result, "the flag must be spent");
        assert!(
            nav.pending_teleport.is_none(),
            "and it must not jump to a hit for an older query"
        );
    }

    /// Drive one row through egui and click at `aim` (computed from the row's own
    /// rect, since the layout decides where things land).
    fn click_row(aim: impl Fn(egui::Rect) -> egui::Pos2) -> (bool, bool) {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
        let s = GeoSuggestion::plain("Vismarkt, Groningen", 53.2194, 6.5665);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        let mut target = egui::Pos2::ZERO;
        let (mut clicked, mut routed) = (false, false);
        for frame in 0..5 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            match frame {
                1 => raw.events.push(egui::Event::PointerMoved(target)),
                2 => raw.events.push(button(target, true)),
                3 => raw.events.push(button(target, false)),
                _ => {}
            }
            let _ = ctx.run_ui(raw, |ui| {
                let pick = destination_row(ui, &s, Some((53.0, 6.0)), 360.0, false);
                target = aim(pick.response.rect);
                clicked |= pick.clicked();
                routed |= pick.routed();
            });
        }
        (clicked, routed)
    }

    /// The ⚑ sits *inside* a widget that senses clicks over the whole row, which is
    /// the arrangement that eats a button's press when it goes the other way round
    /// (see the `dnd_drag_source` entry in CLAUDE.md). It works here because egui
    /// registers a `UiBuilder::sense` scope's own widget *before* its contents and
    /// only updates it in place afterwards, so the button is on top of the row
    /// rather than under it — a fact worth a test rather than a comment, since
    /// nothing about the code says it and the failure is a dead button.
    #[test]
    fn the_route_button_is_clickable_inside_the_row() {
        // The button is on the row's first line, hard against the right edge.
        let hit = [6.0_f32, 10.0, 14.0].iter().any(|&dy| {
            let (clicked, routed) = click_row(|r| egui::pos2(r.right() - 10.0, r.top() + dy));
            routed && !clicked
        });
        assert!(hit, "the ⚑ never received a click");
    }

    /// …and the rest of the row still goes to the row, or picking a search result
    /// would have stopped working.
    #[test]
    fn clicking_the_name_still_picks_the_row() {
        let (clicked, routed) = click_row(|r| egui::pos2(r.left() + 24.0, r.top() + 10.0));
        assert!(clicked, "the row itself must still take a click");
        assert!(!routed, "and must not route");
    }

    #[test]
    fn a_cache_hit_teleports_just_like_a_reply() {
        // Enter pressed before the results landed, then an answer we already
        // hold: the jump must still happen, or Enter would silently do nothing
        // on exactly the queries we serve fastest.
        let mut nav = GeoNavState::default();
        nav.teleport_on_result = true;
        nav.apply_suggestions(vec![suggestion("Groningen, Nederland")]);

        assert!(!nav.teleport_on_result, "flag must be consumed");
        assert!(nav.pending_teleport.is_some(), "cache hit must still jump");
        assert!(!nav.show_suggestions);
        assert_eq!(nav.query, "Groningen, Nederland");
    }

    /// A globe built around [`ANCHOR`], the way `setup_globe` leaves it: Earth's
    /// centre one radius below the local ground.
    fn test_globe() -> GlobeState {
        let mut globe = GlobeState::default();
        globe.radius = 6_378_137.0;
        globe.center_local = Vec3::new(0.0, -globe.radius, 0.0);
        globe.anchor_lat = ANCHOR.0;
        globe.anchor_lng = ANCHOR.1;
        globe.ready = true;
        globe
    }

    /// The separability [`WorldFrame`] is built on, asserted rather than assumed:
    /// longitude may not depend on the northing, nor latitude on the easting.
    ///
    /// This is what lets a grid conversion cost `n + n` instead of `n²`, and it is
    /// exactly the kind of property that is true of the maths, easy to break in the
    /// code, and invisible afterwards — a `lat_at` that quietly read `wx` would
    /// still return plausible coordinates everywhere near the anchor.
    #[test]
    fn the_mercator_frame_is_separable() {
        let frame = WorldFrame::new(ANCHOR);
        for w in [-40_000.0, -1_200.0, 0.0, 850.0, 40_000.0] {
            for other in [-40_000.0, 0.0, 40_000.0] {
                let (_, lng) = world_to_lat_lng(ANCHOR, w, other);
                assert!((lng - frame.lng_at(w)).abs() < 1e-12, "lng moved with wz");
                let (lat, _) = world_to_lat_lng(ANCHOR, other, w);
                assert!((lat - frame.lat_at(w)).abs() < 1e-12, "lat moved with wx");
            }
        }
    }

    /// The frame is an optimisation, so it has to agree with the round trip it
    /// replaced — not merely with itself.
    #[test]
    fn the_frame_round_trips_against_the_forward_mapping() {
        for (lat, lng) in [(53.2194, 6.5665), (53.5, 6.0), (52.9, 7.1)] {
            let (wx, wz) = lat_lng_to_world(ANCHOR, lat, lng);
            let frame = WorldFrame::new(ANCHOR);
            assert!((frame.lat_at(wz) - lat).abs() < 1e-9);
            assert!((frame.lng_at(wx) - lng).abs() < 1e-9);
        }
    }

    /// Near the ground the anchored Mercator frame is the ground truth (the
    /// streamed tiles are keyed with it), so the fix stays on the flat mapping.
    #[test]
    fn camera_geo_uses_the_flat_frame_near_the_ground() {
        let tf = Transform::from_xyz(1_200.0, 400.0, -800.0);
        let got = camera_geo(ANCHOR, &test_globe(), &GridCell::default(), &tf);
        let flat = world_to_lat_lng(ANCHOR, 1_200.0, -800.0);
        assert!((got.0 - flat.0).abs() < 1e-9 && (got.1 - flat.1).abs() < 1e-9);
    }

    /// A Fly pose has to arrive *placed*, not merely written: `camera_controls`
    /// reseeds the look from the live camera transform on the first frame it sees
    /// an uninitialized Fly camera, which threw away the angle the pose had just
    /// set. Every bookmark, `?place=` link and `--shot --look` that named a pitch
    /// was quietly overruled the first time it was applied.
    #[test]
    fn a_fly_pose_arrives_placed_rather_than_reseeded() {
        let mut cs = CameraState::default();
        // Entering Fly clears the flag on purpose — that is what makes a plain
        // mode switch restore the last fly spot.
        crate::systems::free_camera::enter_mode(&mut cs, CameraMode::Free);
        assert!(!cs.free_initialized);

        CameraPose::free_look(-12.0, 180.0).apply(&mut cs);
        assert!(
            cs.free_initialized,
            "a pose that names a look angle must not be reseeded from the transform"
        );
        assert!((cs.free_camera_rotation.x - (-12.0_f32).to_radians()).abs() < 1e-6);
        assert!((cs.free_camera_rotation.y - 180.0_f32.to_radians()).abs() < 1e-6);

        // The other modes are untouched: Orbit still sets its own flag, and a
        // pose with no mode still leaves the current view alone.
        let mut orbit = CameraState::default();
        CameraPose {
            mode: "earth".to_string(),
            orbit_distance: 1_500.0,
            ..Default::default()
        }
        .apply(&mut orbit);
        assert!(orbit.orbit_initialized);
        assert!(!orbit.free_initialized);
    }

    /// In space the flat frame projects a multi-thousand-kilometre offset onto an
    /// arbitrary point of the map — the bug that saved a mid-ocean last location
    /// on exit from orbit. The sub-point is used instead.
    #[test]
    fn camera_geo_uses_the_sub_point_in_space() {
        let globe = test_globe();
        let tf = Transform::from_xyz(3_000_000.0, 1_000_000.0, 0.0);
        let got = camera_geo(ANCHOR, &globe, &GridCell::default(), &tf);

        let dir = (tf.translation.as_dvec3() - globe.center_local.as_dvec3())
            .normalize()
            .as_vec3();
        let expected = sub_point(globe.anchor_lat, globe.anchor_lng, dir);
        assert!((got.0 - expected.0).abs() < 1e-9 && (got.1 - expected.1).abs() < 1e-9);

        // …and that is nowhere near what the flat frame claimed.
        let flat = world_to_lat_lng(ANCHOR, 3_000_000.0, 0.0);
        assert!(
            (flat.1 - got.1).abs() > 5.0,
            "flat frame {flat:?} should disagree wildly with the sub-point {got:?}"
        );
    }

    #[test]
    fn nav_history_back_forward() {
        let mk = |lat: f64| NavEntry {
            lat,
            lng: 0.0,
            altitude: 100.0,
            pose: CameraPose::default(),
            label: None,
        };
        let mut nav = GeoNavState::default();
        assert!(!nav.can_go_back() && !nav.can_go_forward());

        nav.record_visit(mk(1.0));
        assert!(!nav.can_go_back(), "one entry: nowhere to go back");
        nav.record_visit(mk(2.0));
        nav.record_visit(mk(3.0));
        assert_eq!(nav.history_pos, 2);
        assert!(nav.can_go_back() && !nav.can_go_forward());

        // Back to 2.0 queues a flagged teleport (so it isn't re-recorded).
        nav.go_back();
        assert_eq!(nav.history_pos, 1);
        assert!(nav.from_history);
        assert_eq!(nav.pending_teleport.as_ref().unwrap().lat, 2.0);
        assert!(nav.can_go_forward());

        // Forward again to 3.0 (simulate apply_teleport clearing the flag).
        nav.from_history = false;
        nav.pending_teleport = None;
        nav.go_forward();
        assert_eq!(nav.history_pos, 2);
        assert_eq!(nav.pending_teleport.as_ref().unwrap().lat, 3.0);
        assert!(!nav.can_go_forward());

        // Go back, then a fresh (non-history) visit truncates the forward tail
        // (3.0) and appends — browser semantics.
        nav.from_history = false;
        nav.go_back(); // pos 1 → 2.0
        nav.from_history = false; // a real teleport, not a history move
        nav.record_visit(mk(9.0));
        assert_eq!(
            nav.history.iter().map(|e| e.lat).collect::<Vec<_>>(),
            vec![1.0, 2.0, 9.0]
        );
        assert_eq!(nav.history_pos, 2);
        assert!(!nav.can_go_forward());

        // Re-visiting the current spot is a no-op (no duplicate stacking).
        nav.record_visit(mk(9.0));
        assert_eq!(nav.history.len(), 3);
    }

    #[test]
    fn parse_location_variants() {
        let fix = |lat, lng, altitude, look| {
            Some(LocationFix {
                lat,
                lng,
                altitude,
                look,
            })
        };
        assert_eq!(
            parse_location("53.2194, 6.5665"),
            fix(53.2194, 6.5665, None, None)
        );
        assert_eq!(
            parse_location("53.21940, 6.56650, 120"),
            fix(53.2194, 6.5665, Some(120.0), None)
        );
        assert_eq!(
            parse_location("53.2194, 6.5665, 120m"),
            fix(53.2194, 6.5665, Some(120.0), None)
        );
        assert_eq!(
            parse_location("53.2194N 6.5665E"),
            fix(53.2194, 6.5665, None, None)
        );
        assert_eq!(parse_location("Groningen"), None);
        assert_eq!(parse_location("300, 4"), None); // latitude out of range
        assert_eq!(parse_location("1, 2, 3, 4"), None); // a lone pitch is malformed
    }

    #[test]
    fn parse_location_with_camera_rotation() {
        // What the HUD copy button writes, decoration and all.
        assert_eq!(
            parse_location("53.21940, 6.56650, 120m, -12.5°, 45.0°"),
            Some(LocationFix {
                lat: 53.2194,
                lng: 6.5665,
                altitude: Some(120.0),
                look: Some((-12.5, 45.0)),
            })
        );
        // And the same string as it survives a URL, without the units.
        assert_eq!(
            parse_location("53.2194,6.5665,120,-12.5,45"),
            parse_location("53.21940, 6.56650, 120m, -12.5°, 45.0°")
        );
        assert_eq!(parse_location("53.2194, 6.5665, 120, x, 45"), None);
    }

    #[test]
    fn location_fix_teleport_restores_the_view() {
        let fix = parse_location("53.2194, 6.5665, 120, -12.5, 45").unwrap();
        let tp = Teleport::fix(fix);
        assert_eq!(tp.altitude, Some(120.0));
        let mut cs = CameraState::default();
        tp.pose.unwrap().apply(&mut cs);
        assert!(matches!(cs.mode, CameraMode::Free));
        assert!((cs.free_camera_rotation.x - (-12.5f32).to_radians()).abs() < 1e-6);
        assert!((cs.free_camera_rotation.y - 45f32.to_radians()).abs() < 1e-6);
        // A fix without angles leaves the camera alone.
        assert!(Teleport::fix(parse_location("53.2194, 6.5665").unwrap())
            .pose
            .is_none());
    }

    #[test]
    fn camera_pose_round_trips() {
        let cs = CameraState {
            mode: CameraMode::Free,
            free_camera_rotation: Vec2::new(0.3, -1.2),
            azimuth: 1.1,
            pitch: 0.4,
            distance: 42.0,
            ..Default::default()
        };
        let pose = CameraPose::capture(&cs);
        let mut restored = CameraState::default();
        pose.apply(&mut restored);
        assert!(matches!(restored.mode, CameraMode::Free));
        assert_eq!(restored.free_camera_rotation, Vec2::new(0.3, -1.2));
        assert_eq!(restored.azimuth, 1.1);
        assert_eq!(restored.pitch, 0.4);
        assert_eq!(restored.distance, 42.0);
    }

    #[test]
    fn earth_pose_round_trips() {
        let cs = CameraState {
            mode: CameraMode::Orbit,
            orbit_distance: 8200.0,
            orbit_yaw: 1.3,
            orbit_pitch: 0.9,
            ..Default::default()
        };
        let pose = CameraPose::capture(&cs);
        let mut restored = CameraState::default();
        pose.apply(&mut restored);
        assert!(matches!(restored.mode, CameraMode::Orbit));
        assert_eq!(restored.orbit_distance, 8200.0);
        assert_eq!(restored.orbit_yaw, 1.3);
        assert_eq!(restored.orbit_pitch, 0.9);
        // The focus is re-derived from the jump target, so the pose leaves it for
        // apply_teleport to set; entering marks the state seeded.
        assert!(restored.orbit_initialized);
    }

    #[test]
    fn empty_pose_keeps_current_view() {
        // Old bookmarks (saved before poses) deserialize to an empty mode and
        // must not disturb the current camera.
        let mut cs = CameraState {
            mode: CameraMode::Avatar,
            azimuth: 0.7,
            ..Default::default()
        };
        CameraPose::default().apply(&mut cs);
        assert!(matches!(cs.mode, CameraMode::Avatar));
        assert_eq!(cs.azimuth, 0.7);
    }
}
