//! Geo navigation: a browser-style search bar in the top bar.
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
use crate::resources::{AvatarState, CameraMode, CameraState};
use crate::systems::free_camera::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::map_stream::{lat_lng_to_tile_frac, MapStream};
use crate::systems::theme;
use viberfox_core::world::tile_to_meters;

/// Nominatim base URL. Defaults to the project's self-hosted instance (seeded with
/// the Netherlands extract — see `docs/guides/server.md`). Override with
/// `VIBE_NOMINATIM_URL`, e.g. the public `https://nominatim.openstreetmap.org`.
const DEFAULT_NOMINATIM_URL: &str = "https://nominatim.garage44.eu";

/// Resolve the Nominatim base URL (no trailing slash) from `VIBE_NOMINATIM_URL`,
/// falling back to the self-hosted default. Callers append `/search`, `/reverse`, …
pub(crate) fn nominatim_base_url() -> String {
    std::env::var("VIBE_NOMINATIM_URL")
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
/// polite when `VIBE_NOMINATIM_URL` points back at `nominatim.openstreetmap.org`.
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
/// Avatar rest height after a teleport. The humanoid models are authored
/// origin-at-feet, so this is the ground/tile surface (`avatar::GROUND_HEIGHT`);
/// the offline gravity clamp rests the feet at the same value. (Was 0.45 = ground
/// + half fox height, which floated the humanoids ~0.4 m.)
const AVATAR_GROUND_Y: f64 = 0.05;
/// Minimum free-camera altitude after a teleport, so the destination is visible.
const MIN_TELEPORT_ALTITUDE: f64 = 60.0;

/// One hit in a geocoding dropdown.
///
/// Nominatim answers with a single `display_name` comma-run — "Amsterdam, North
/// Holland, Netherlands" — which is the whole address flattened into a line. Shown
/// as one truncated string it hides exactly the part that tells two hits apart:
/// the eight Amsterdams the public index returns differ only past the first comma.
/// So the hit is split at parse time into what it *is* ([`name`](Self::name)), what
/// disambiguates it ([`context`](Self::context)) and what kind of thing it is
/// ([`kind`](Self::kind)); `label` stays the full line, because that is what goes
/// back into the address bar when the hit is picked.
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

#[derive(Resource)]
pub struct GeoNavState {
    pub query: String,
    suggestions: Vec<GeoSuggestion>,
    show_suggestions: bool,
    error: Option<String>,
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
    /// Browser-style location history; `history[history_pos]` is where we are.
    /// Every teleport that isn't itself a Back/Forward move truncates the
    /// forward entries and appends the destination (see `apply_teleport`).
    pub history: Vec<NavEntry>,
    pub history_pos: usize,
    /// The pending teleport is a Back/Forward move — `apply_teleport` must not
    /// re-record it as a new visit.
    from_history: bool,
    /// The address bar should grab keyboard focus next frame (set by the top
    /// bar's Ctrl+L shortcut and by [`seed_search`](Self::seed_search), consumed
    /// in `nav_bar_ui`); also unfolds the bar, since a field that is not drawn
    /// cannot take focus.
    pub focus_search: bool,
    /// The address bar currently holds keyboard focus — read by `ui_owns_keyboard`
    /// so typing a search doesn't also walk the avatar (set in `nav_bar_ui`).
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
    fn apply_suggestions(&mut self, list: Vec<GeoSuggestion>) {
        self.error = list.is_empty().then(|| "No results".to_string());
        if self.teleport_on_result {
            self.teleport_on_result = false;
            if let Some(first) = list.first() {
                self.pending_teleport = Some(Teleport::geo(first.lat, first.lng, None));
                self.query = first.label.clone();
                self.show_suggestions = false;
            } else {
                self.show_suggestions = true;
            }
        } else {
            self.show_suggestions = true;
        }
        self.suggestions = list;
    }

    /// Drop the suggestion list. Called when the address bar folds away — the
    /// dropdown is an `Area` of its own and would otherwise hang under a bar that
    /// is no longer drawn.
    pub fn close_suggestions(&mut self) {
        self.show_suggestions = false;
        self.teleport_on_result = false;
    }

    /// Put a query in the address bar and open its dropdown, as if it had been
    /// typed. The debounce and the "has the text changed" gate both live behind
    /// private fields, so a scripted run (`--ui search`) cannot reach the search
    /// path without this — and the address bar is otherwise uncapturable, since
    /// revealing it needs a pointer at the top edge.
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
}

impl Teleport {
    /// A plain geo jump that keeps the current camera orientation.
    pub fn geo(lat: f64, lng: f64, altitude: Option<f64>) -> Self {
        Self {
            lat,
            lng,
            altitude,
            pose: None,
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

impl CameraPose {
    /// Capture the current camera orientation.
    pub fn capture(cs: &CameraState) -> Self {
        Self {
            mode: match cs.mode {
                CameraMode::Free => "free",
                CameraMode::Avatar => "avatar",
                CameraMode::Orbit => "earth",
            }
            .to_string(),
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
        cs.free_camera_rotation = Vec2::new(self.free_rotation[0], self.free_rotation[1]);
        cs.azimuth = self.azimuth;
        cs.pitch = self.pitch;
        cs.distance = self.distance;
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
            last_edit: None,
            last_sent: String::new(),
            last_sent_at: None,
            in_flight: 0,
            cache: Vec::new(),
            teleport_on_result: false,
            force_search: false,
            results: Arc::new(Mutex::new(Vec::new())),
            pending_teleport: None,
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

/// Inverse of [`lat_lng_to_world`].
pub fn world_to_lat_lng(anchor: (f64, f64, u32), wx: f64, wz: f64) -> (f64, f64) {
    let (clat, clng, zoom) = anchor;
    let gs = tile_to_meters(zoom, clat);
    let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, zoom);
    let fx = wx / gs + fcx;
    let fy = wz / gs + fcy;
    let n = 2.0_f64.powi(zoom as i32);
    let lng = fx / n * 360.0 - 180.0;
    let lat = ((1.0 - 2.0 * fy / n) * std::f64::consts::PI)
        .sinh()
        .atan()
        .to_degrees();
    (lat, lng)
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

/// Where this session should start, if the launch asked for a specific place:
/// `?place=53.21940,6.56650,120m,-12.5,45` on the web, `VIBERFOX_PLACE` in the
/// environment on desktop. The value is exactly what the HUD's ⧉ button copies,
/// so a shared place is a shareable URL.
///
/// Takes precedence over the persisted last location (see
/// `user_store::restore_start_location`) — a link that names a place and then
/// drops you where you left off would be no link at all.
pub fn launch_place() -> Option<LocationFix> {
    let raw = crate::env_value("VIBERFOX_PLACE")?;
    let fix = parse_location(&raw);
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
async fn geocode_fetch(url: String) -> Result<Vec<GeoSuggestion>, String> {
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
/// A letter and not an arrow glyph on purpose: the loaded font has none of
/// `← → ↑ ↓ ↖ ↗ ↘ ↙` (see [`theme::first_drawable`]), so a direction drawn as an
/// arrow would be a row of empty boxes.
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

/// One suggestion row, shared by the address bar's dropdown and the route panel's
/// endpoint fields so the two lists cannot drift apart.
///
/// Two lines: the name (plus what kind of place it is) leads, and the address
/// context and the distance from `viewer` sit under it in the dim tone. The whole
/// block is one click target — `UiBuilder::sense` puts a single response over the
/// laid-out content, which a `Button` could not do without collapsing both lines
/// into one string and losing the type hierarchy that makes the row scannable.
pub fn suggestion_row(
    ui: &mut egui::Ui,
    s: &GeoSuggestion,
    viewer: Option<(f64, f64)>,
    width: f32,
) -> egui::Response {
    // Reserve the highlight's place in the paint list *before* the content, so the
    // fill lands behind the text rather than over it.
    let bg = ui.painter().add(egui::Shape::Noop);
    let inner = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
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
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if let Some(here) = viewer {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(distance_label(here, (s.lat, s.lng)))
                            .small()
                            .color(theme::FOX_TEXT),
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
    });
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
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Poll finished lookups into the UI state and send the debounced next query.
pub fn drive_geocode(mut nav: ResMut<GeoNavState>) {
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
    let eligible = fresh && query.len() >= MIN_QUERY_LEN && parse_location(&query).is_none();

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

// ─── Search bar UI (rendered inside the top menu bar) ────────────────────────

/// Width of the address field. Narrower than it was: the bar is a floating card
/// sized to its content now, not a full-width strip, so every point here is a
/// point of map it covers. Long place names still fit — the *dropdown* is what
/// has to be readable, and it is wider than the field on purpose.
const SEARCH_FIELD_WIDTH: f32 = 215.0;
/// Widest the suggestion dropdown gets. Wider than the field — the rows carry an
/// address line the field never has to show.
const SUGGESTION_POPUP_WIDTH: f32 = 380.0;

/// Draw the address bar and its dropdown. `viewer` is where the camera is on
/// Earth, used to say how far away each hit is; `None` before the world anchor
/// exists, and the rows simply omit the distance.
pub fn nav_bar_ui(ui: &mut egui::Ui, nav: &mut GeoNavState, viewer: Option<(f64, f64)>) {
    let field = ui.add(
        egui::TextEdit::singleline(&mut nav.query)
            .desired_width(SEARCH_FIELD_WIDTH)
            // Vertical padding so the field matches the taller menu bar.
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
        ui.spinner();
    }

    if field.changed() {
        nav.last_edit = Some(Instant::now());
        nav.teleport_on_result = false;
        nav.error = None;
        nav.show_suggestions = true;
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
        if let Some(fix) = parse_location(&nav.query) {
            nav.pending_teleport = Some(Teleport::fix(fix));
            nav.show_suggestions = false;
        } else if nav.query.trim() == nav.last_sent && !nav.suggestions.is_empty() {
            let first = nav.suggestions[0].clone();
            nav.pending_teleport = Some(Teleport::geo(first.lat, first.lng, None));
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

    // Suggestion dropdown under the field. Pasted coordinates get a single
    // direct "Go to" entry instead of a lookup.
    let coord_fix = parse_location(&nav.query);
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
    let has_content = coord_entry.is_some() || !nav.suggestions.is_empty() || nav.error.is_some();
    let show_popup =
        (nav.show_suggestions || (field.has_focus() && coord_entry.is_some())) && has_content;
    if !show_popup {
        return;
    }

    let mut picked: Option<GeoSuggestion> = None;
    let mut popup_hovered = false;
    egui::Area::new(egui::Id::new("geo_nav_suggestions"))
        .order(egui::Order::Foreground)
        .fixed_pos(field.rect.left_bottom() + egui::vec2(0.0, 4.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let width = field
                    .rect
                    .width()
                    .max(SUGGESTION_POPUP_WIDTH)
                    .min(ui.ctx().viewport_rect().width() - 32.0);
                ui.set_min_width(width);
                ui.set_max_width(width);
                let entries: Vec<&GeoSuggestion> =
                    coord_entry.iter().chain(nav.suggestions.iter()).collect();
                for (i, s) in entries.iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    if suggestion_row(ui, s, viewer, width).clicked() {
                        picked = Some((*s).clone());
                    }
                }
                if let Some(err) = &nav.error {
                    ui.weak(err.as_str());
                }
                popup_hovered = ui.ui_contains_pointer();
            });
        });

    if let Some(s) = picked {
        // The direct coordinate entry keeps its pasted altitude and view; a
        // Nominatim hit is only a spot. It also leaves the field alone: the user
        // typed those coordinates, and replacing them with "Go to 53.21940, …"
        // would make a re-submit unparseable.
        let fix = coord_fix.filter(|f| f.lat == s.lat && f.lng == s.lng);
        match fix {
            Some(fix) => nav.pending_teleport = Some(Teleport::fix(fix)),
            None => {
                nav.pending_teleport = Some(Teleport::geo(s.lat, s.lng, None));
                nav.query = s.label;
            }
        }
        nav.show_suggestions = false;
        nav.teleport_on_result = false;
    } else if field.lost_focus() && !submitted && !popup_hovered {
        nav.show_suggestions = false;
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
    stream.request_reanchor(lat, lng);
    let base_zoom = stream
        .geo_anchor()
        .map(|(_, _, z)| z)
        .unwrap_or(viberfox_core::world::REGION_ZOOM_LEVEL);
    let snapped = crate::systems::map_stream::snap_anchor(lat, lng, base_zoom);
    let (wx, wz) = lat_lng_to_world((snapped.0, snapped.1, base_zoom), lat, lng);
    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);

    let (avatar_cell, avatar_local) = grid.translation_to_grid(DVec3::new(wx, AVATAR_GROUND_Y, wz));
    if let Ok((mut cell, mut tf)) = avatars.single_mut() {
        *cell = avatar_cell;
        tf.translation = avatar_local;
    }
    avatar_state.position = avatar_local;
    avatar_state.display_position = avatar_local;

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
            CameraMode::Free => {
                let altitude = target_alt
                    .unwrap_or_else(|| {
                        cell.y as f64 * WORLD_CELL_EDGE as f64 + tf.translation.y as f64
                    })
                    .max(MIN_TELEPORT_ALTITUDE);
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
        });
    }

    tracing::info!(lat, lng, "teleported to geo target");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::globe::{sub_point, GlobeState};

    const ANCHOR: (f64, f64, u32) = (53.2194, 6.5665, 16);

    fn suggestion(label: &str) -> GeoSuggestion {
        GeoSuggestion::plain(label, 53.0, 6.0)
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
        // in the address bar.
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

    /// Near the ground the anchored Mercator frame is the ground truth (the
    /// streamed tiles are keyed with it), so the fix stays on the flat mapping.
    #[test]
    fn camera_geo_uses_the_flat_frame_near_the_ground() {
        let tf = Transform::from_xyz(1_200.0, 400.0, -800.0);
        let got = camera_geo(ANCHOR, &test_globe(), &GridCell::default(), &tf);
        let flat = world_to_lat_lng(ANCHOR, 1_200.0, -800.0);
        assert!((got.0 - flat.0).abs() < 1e-9 && (got.1 - flat.1).abs() < 1e-9);
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
