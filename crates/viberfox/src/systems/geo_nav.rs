//! Geo navigation: a browser-style search bar in the top bar.
//!
//! Type a place name ("Groningen") to get OpenStreetMap Nominatim suggestions
//! and click one to teleport the avatar/camera there; or paste raw
//! coordinates — "lat, lng" or "lat, lng, altitude" (the format the HUD's
//! copy button produces) — to jump directly without a lookup. Teleports keep
//! the current camera mode; bookmark jumps and the last-location restore
//! (`user_store`) go through the same [`GeoNavState::pending_teleport`] path.
//!
//! Nominatim usage policy (https://operations.osmfoundation.org/policies/nominatim/):
//! requests are debounced behind a typing pause, serialized (one in flight,
//! ≥1 s apart) and sent with an identifying User-Agent.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::{Grid, GridCell};
use serde::{Deserialize, Serialize};

use crate::components::Avatar;
use crate::resources::{AvatarState, CameraMode, CameraState};
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::map_stream::{lat_lng_to_tile_frac, MapStream};
use vibe_core::world::tile_to_meters;

/// Identifying User-Agent — Nominatim's policy blocks requests without one.
const NAV_USER_AGENT: &str =
    "viberfox/0.1 (geocoding; https://github.com/garage44/viberfox)";
/// Typing pause before a query is sent.
const DEBOUNCE: Duration = Duration::from_millis(450);
/// Nominatim allows at most one request per second.
const MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(1);
const MIN_QUERY_LEN: usize = 2;
const MAX_SUGGESTIONS: usize = 8;
/// Avatar rest height after a teleport (ground top + half fox height; offline
/// gravity clamps to the same value).
const AVATAR_GROUND_Y: f64 = 0.45;
/// Minimum free-camera altitude after a teleport, so the destination is visible.
const MIN_TELEPORT_ALTITUDE: f64 = 60.0;

#[derive(Clone)]
pub struct GeoSuggestion {
    pub label: String,
    pub lat: f64,
    pub lng: f64,
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
    in_flight: bool,
    /// Enter was pressed before results arrived: jump to the top hit when they do.
    teleport_on_result: bool,
    /// Enter re-submits a query even if it equals the last sent one.
    force_search: bool,
    results: Arc<Mutex<Option<GeocodeReply>>>,
    /// Target picked from a suggestion, pasted coordinates, a bookmark, or the
    /// startup restore. Consumed by `apply_teleport` (kept until the world
    /// anchor exists).
    pub pending_teleport: Option<Teleport>,
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
        Self { lat, lng, altitude, pose: None }
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
            in_flight: false,
            teleport_on_result: false,
            force_search: false,
            results: Arc::new(Mutex::new(None)),
            pending_teleport: None,
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

/// Parse a pasted coordinate pair: "53.21940, 6.56650", "53.2194 6.5665",
/// "53.2194N, 6.5665E", "(-33.9, 18.4)". Returns (lat, lng) when both values
/// are in range.
pub fn parse_lat_lng(input: &str) -> Option<(f64, f64)> {
    fn coord(s: &str, pos: char, neg: char) -> Option<f64> {
        let s = s.trim();
        let (body, sign) = if let Some(b) =
            s.strip_suffix([pos, pos.to_ascii_lowercase()])
        {
            (b, 1.0)
        } else if let Some(b) = s.strip_suffix([neg, neg.to_ascii_lowercase()]) {
            (b, -1.0)
        } else {
            (s, 1.0)
        };
        body.trim().parse::<f64>().ok().map(|v| v * sign)
    }

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
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng))
        .then_some((lat, lng))
}

/// Like [`parse_lat_lng`], but also accepts a third altitude component
/// ("53.2194, 6.5665, 120" or "… 120m"), as produced by the HUD copy button.
pub fn parse_location(input: &str) -> Option<(f64, f64, Option<f64>)> {
    if let Some((lat, lng)) = parse_lat_lng(input) {
        return Some((lat, lng, None));
    }
    let (coords, alt) = input.trim().rsplit_once([',', ';'])?;
    let alt: f64 = alt.trim().trim_end_matches(['m', 'M']).trim().parse().ok()?;
    let (lat, lng) = parse_lat_lng(coords)?;
    Some((lat, lng, Some(alt.clamp(1.0, 100_000.0))))
}

// ─── Nominatim lookup (background thread) ────────────────────────────────────

/// Percent-encode a URL query component (RFC 3986 unreserved set kept).
fn percent_encode(s: &str) -> String {
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

fn spawn_geocode(query: String, results: Arc<Mutex<Option<GeocodeReply>>>) {
    std::thread::spawn(move || {
        let url = format!(
            "https://nominatim.openstreetmap.org/search?format=jsonv2&limit={}&q={}",
            MAX_SUGGESTIONS,
            percent_encode(&query)
        );
        let result = (|| -> Result<Vec<GeoSuggestion>, String> {
            let body = ureq::get(&url)
                .set("User-Agent", NAV_USER_AGENT)
                .call()
                .map_err(|e| e.to_string())?
                .into_string()
                .map_err(|e| e.to_string())?;
            let json: serde_json::Value =
                serde_json::from_str(&body).map_err(|e| e.to_string())?;
            let items = json.as_array().ok_or("unexpected Nominatim response")?;
            Ok(items
                .iter()
                .filter_map(|it| {
                    Some(GeoSuggestion {
                        label: it.get("display_name")?.as_str()?.to_string(),
                        lat: it.get("lat")?.as_str()?.parse().ok()?,
                        lng: it.get("lon")?.as_str()?.parse().ok()?,
                    })
                })
                .collect())
        })();
        if let Err(ref e) = result {
            tracing::warn!(query = %query, error = %e, "geocoding lookup failed");
        }
        if let Ok(mut guard) = results.lock() {
            *guard = Some(GeocodeReply { query, result });
        }
    });
}

/// Poll finished lookups into the UI state and send the debounced next query.
pub fn drive_geocode(mut nav: ResMut<GeoNavState>) {
    // Harvest a finished lookup (drop it silently if the query has moved on —
    // the debounce below will send the newer one).
    let reply = nav.results.lock().ok().and_then(|mut g| g.take());
    if let Some(reply) = reply {
        nav.in_flight = false;
        if reply.query == nav.query.trim() {
            match reply.result {
                Ok(list) => {
                    nav.error = list.is_empty().then(|| "No results".to_string());
                    if nav.teleport_on_result {
                        nav.teleport_on_result = false;
                        if let Some(first) = list.first() {
                            nav.pending_teleport = Some(Teleport::geo(first.lat, first.lng, None));
                            nav.query = first.label.clone();
                            nav.show_suggestions = false;
                        } else {
                            nav.show_suggestions = true;
                        }
                    } else {
                        nav.show_suggestions = true;
                    }
                    nav.suggestions = list;
                }
                Err(e) => {
                    nav.suggestions.clear();
                    nav.error = Some(format!("Search failed: {e}"));
                    nav.show_suggestions = true;
                    nav.teleport_on_result = false;
                }
            }
        }
    }

    let query = nav.query.trim().to_string();
    let debounced = nav
        .last_edit
        .is_some_and(|t| t.elapsed() >= DEBOUNCE);
    let throttled = nav
        .last_sent_at
        .is_some_and(|t| t.elapsed() < MIN_REQUEST_INTERVAL);
    let fresh = query != nav.last_sent || nav.force_search;
    if !nav.in_flight
        && !throttled
        && (debounced || nav.force_search)
        && fresh
        && query.len() >= MIN_QUERY_LEN
        && parse_location(&query).is_none()
    {
        nav.in_flight = true;
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

pub fn nav_bar_ui(ui: &mut egui::Ui, nav: &mut GeoNavState) {
    let field = ui.add(
        egui::TextEdit::singleline(&mut nav.query)
            .desired_width(340.0)
            // Vertical padding so the field matches the taller menu bar.
            .margin(egui::Margin::symmetric(8, 6))
            .hint_text("Search place, or paste \"lat, lng\"…"),
    );
    if nav.in_flight {
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
            state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(len),
            )));
            state.store(ui.ctx(), field.id);
        }
        if !nav.suggestions.is_empty() {
            nav.show_suggestions = true;
        }
    }

    let submitted =
        field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if submitted {
        if let Some((lat, lng, alt)) = parse_location(&nav.query) {
            nav.pending_teleport = Some(Teleport::geo(lat, lng, alt));
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
    let coord_alt = parse_location(&nav.query);
    let coord_entry = coord_alt.map(|(lat, lng, alt)| GeoSuggestion {
        label: match alt {
            Some(a) => format!("Go to {:.5}, {:.5} at {:.0} m", lat, lng, a),
            None => format!("Go to {:.5}, {:.5}", lat, lng),
        },
        lat,
        lng,
    });
    let has_content = coord_entry.is_some()
        || !nav.suggestions.is_empty()
        || nav.error.is_some();
    let show_popup =
        (nav.show_suggestions || (field.has_focus() && coord_entry.is_some()))
            && has_content;
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
                ui.set_min_width(field.rect.width());
                ui.set_max_width(420.0);
                let entries: Vec<&GeoSuggestion> = coord_entry
                    .iter()
                    .chain(nav.suggestions.iter())
                    .collect();
                for s in entries {
                    let resp = ui.add(
                        egui::Button::new(&s.label)
                            .wrap_mode(egui::TextWrapMode::Truncate)
                            .min_size(egui::vec2(field.rect.width(), 0.0)),
                    );
                    if resp.clicked() {
                        picked = Some(s.clone());
                    }
                }
                if let Some(err) = &nav.error {
                    ui.weak(err.as_str());
                }
                popup_hovered = ui.ui_contains_pointer();
            });
        });

    if let Some(s) = picked {
        // The direct coordinate entry keeps its pasted altitude.
        let alt = coord_alt
            .filter(|(lat, lng, _)| *lat == s.lat && *lng == s.lng)
            .and_then(|(_, _, a)| a);
        nav.pending_teleport = Some(Teleport::geo(s.lat, s.lng, alt));
        nav.query = s.label;
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
pub fn apply_camera_reanchor(
    mut camera_state: ResMut<CameraState>,
    mut nav: ResMut<GeoNavState>,
) {
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
    mut cameras: Query<
        (&mut GridCell, &mut Transform),
        (With<FreeCamera>, Without<Avatar>),
    >,
    mut avatars: Query<
        (&mut GridCell, &mut Transform),
        (With<Avatar>, Without<FreeCamera>),
    >,
) {
    if stream.geo_anchor().is_none() || nav.pending_teleport.is_none() {
        return; // nothing to do, or world not initialized yet (stay pending)
    }
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
        .unwrap_or(vibe_core::world::REGION_ZOOM_LEVEL);
    let snapped = crate::systems::map_stream::snap_anchor(lat, lng, base_zoom);
    let (wx, wz) = lat_lng_to_world((snapped.0, snapped.1, base_zoom), lat, lng);
    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);

    let (avatar_cell, avatar_local) =
        grid.translation_to_grid(DVec3::new(wx, AVATAR_GROUND_Y, wz));
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
                let (new_cell, local) =
                    grid.translation_to_grid(DVec3::new(wx, altitude, wz));
                *cell = new_cell;
                tf.translation = local;
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

    tracing::info!(lat, lng, "teleported to geo target");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_location_variants() {
        assert_eq!(parse_location("53.2194, 6.5665"), Some((53.2194, 6.5665, None)));
        assert_eq!(
            parse_location("53.21940, 6.56650, 120"),
            Some((53.2194, 6.5665, Some(120.0)))
        );
        assert_eq!(
            parse_location("53.2194, 6.5665, 120m"),
            Some((53.2194, 6.5665, Some(120.0)))
        );
        assert_eq!(parse_location("53.2194N 6.5665E"), Some((53.2194, 6.5665, None)));
        assert_eq!(parse_location("Groningen"), None);
        assert_eq!(parse_location("300, 4"), None); // latitude out of range
        assert_eq!(parse_location("1, 2, 3, 4"), None);
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
