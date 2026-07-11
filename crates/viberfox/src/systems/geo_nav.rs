//! Geo navigation: a browser-style search bar in the top bar, plus a
//! bottom-right geo-location + compass overlay.
//!
//! Type a place name ("Groningen") to get OpenStreetMap Nominatim suggestions
//! and click one to teleport the avatar/camera there; or paste a raw
//! "lat, lng" pair (the format the overlay's copy button produces) to jump
//! directly without a lookup.
//!
//! Nominatim usage policy (https://operations.osmfoundation.org/policies/nominatim/):
//! requests are debounced behind a typing pause, serialized (one in flight,
//! ≥1 s apart) and sent with an identifying User-Agent.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::{Grid, GridCell};

use crate::components::Avatar;
use crate::resources::{AvatarState, CameraMode, CameraState};
use crate::systems::egui_manager::EguiManager;
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
    /// Target picked from a suggestion or pasted coordinates; consumed by
    /// `apply_teleport` (kept until the world anchor exists).
    pub pending_teleport: Option<(f64, f64)>,
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
                            nav.pending_teleport = Some((first.lat, first.lng));
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
        && parse_lat_lng(&query).is_none()
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
    if field.gained_focus() && !nav.suggestions.is_empty() {
        nav.show_suggestions = true;
    }

    let submitted =
        field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if submitted {
        if let Some(target) = parse_lat_lng(&nav.query) {
            nav.pending_teleport = Some(target);
            nav.show_suggestions = false;
        } else if nav.query.trim() == nav.last_sent && !nav.suggestions.is_empty() {
            let first = nav.suggestions[0].clone();
            nav.pending_teleport = Some((first.lat, first.lng));
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
    let coord_entry = parse_lat_lng(&nav.query).map(|(lat, lng)| GeoSuggestion {
        label: format!("Go to {:.5}, {:.5}", lat, lng),
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
        nav.pending_teleport = Some((s.lat, s.lng));
        nav.query = s.label;
        nav.show_suggestions = false;
        nav.teleport_on_result = false;
    } else if field.lost_focus() && !submitted && !popup_hovered {
        nav.show_suggestions = false;
    }
}

// ─── Teleport ────────────────────────────────────────────────────────────────

/// Move the avatar and camera to a picked geo target. Runs once the world
/// anchor exists (the target stays pending until then). Positions are set as
/// big_space `GridCell` + local translation, the same convention the streamed
/// map uses, so tiles and buildings load around the destination automatically.
/// Online, the sim still owns the avatar, so the server may pull it back.
pub fn apply_teleport(
    mut nav: ResMut<GeoNavState>,
    stream: Res<MapStream>,
    camera_state: Res<CameraState>,
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
    let Some((lat, lng)) = nav.pending_teleport else {
        return;
    };
    let Some(anchor) = stream.geo_anchor() else {
        return; // world not initialized yet; keep the target pending
    };
    nav.pending_teleport = None;

    let (wx, wz) = lat_lng_to_world(anchor, lat, lng);
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
            // Keep the current altitude (with a floor) over the new spot.
            CameraMode::Free => {
                let altitude = (cell.y as f64 * WORLD_CELL_EDGE as f64
                    + tf.translation.y as f64)
                    .max(MIN_TELEPORT_ALTITUDE);
                let (new_cell, local) =
                    grid.translation_to_grid(DVec3::new(wx, altitude, wz));
                *cell = new_cell;
                tf.translation = local;
            }
        }
    }

    tracing::info!(lat, lng, "teleported to geo target");
}

// ─── Geo-location + compass overlay (bottom-right) ───────────────────────────

pub fn render_geo_overlay(
    mut egui: ResMut<EguiManager>,
    stream: Res<MapStream>,
    cameras: Query<(&GridCell, &Transform), With<FreeCamera>>,
    mut copied_at: Local<Option<Instant>>,
) {
    let Some(anchor) = stream.geo_anchor() else {
        return;
    };
    let Ok((cell, tf)) = cameras.single() else {
        return;
    };
    let wx = cell.x as f64 * WORLD_CELL_EDGE as f64 + tf.translation.x as f64;
    let wz = cell.z as f64 * WORLD_CELL_EDGE as f64 + tf.translation.z as f64;
    let (lat, lng) = world_to_lat_lng(anchor, wx, wz);

    // Compass heading of the view direction: 0° = north (−z), 90° = east (+x).
    let forward = tf.rotation * Vec3::NEG_Z;
    let heading = f32::atan2(forward.x, -forward.z);

    let ctx = egui.ctx_mut();
    egui::Area::new(egui::Id::new("geo_overlay"))
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-8.0, -8.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    compass(ui, heading);
                    let text = format!("{:.5}, {:.5}", lat, lng);
                    ui.label(egui::RichText::new(&text).monospace());
                    let copied =
                        copied_at.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5);
                    let button = ui
                        .add(egui::Button::new(if copied { "✔" } else { "⧉" }).frame(false))
                        .on_hover_text("Copy location (paste it in the search bar to return here)");
                    if button.clicked() {
                        crate::utils::clipboard::set_text(&text);
                        *copied_at = Some(Instant::now());
                    }
                });
            });
        });
}

/// A small compass rose: red needle points to world north, grey tail south,
/// with the "N" riding the rim. Hover shows the numeric heading.
fn compass(ui: &mut egui::Ui, heading: f32) {
    let size = 30.0;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    resp.on_hover_text(format!(
        "Heading {:.0}°",
        heading.to_degrees().rem_euclid(360.0)
    ));
    let painter = ui.painter_at(rect);
    let center = rect.center();
    let radius = size * 0.5 - 1.0;

    painter.circle(
        center,
        radius,
        egui::Color32::from_black_alpha(110),
        egui::Stroke::new(1.0, egui::Color32::from_gray(140)),
    );

    // Screen direction of world north: straight up when facing north.
    let a = -heading;
    let north = egui::vec2(a.sin(), -a.cos());
    let perp = egui::vec2(-north.y, north.x);
    let needle = radius - 8.0;
    let half_width = 3.0;
    painter.add(egui::Shape::convex_polygon(
        vec![
            center + north * needle,
            center + perp * half_width,
            center - perp * half_width,
        ],
        egui::Color32::from_rgb(225, 70, 55),
        egui::Stroke::NONE,
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![
            center - north * needle,
            center - perp * half_width,
            center + perp * half_width,
        ],
        egui::Color32::from_gray(150),
        egui::Stroke::NONE,
    ));
    painter.circle_filled(center, 1.5, egui::Color32::WHITE);
    painter.text(
        center + north * (radius - 4.0),
        egui::Align2::CENTER_CENTER,
        "N",
        egui::FontId::proportional(8.0),
        egui::Color32::WHITE,
    );
}
