//! Bookmarks: a side panel (top bar ▸ Bookmarks) listing saved locations, and
//! the machinery behind the HUD's ★ button.
//!
//! A new bookmark is named by reverse-geocoding its coordinates through
//! Nominatim (same usage policy as the search bar: identifying User-Agent,
//! background thread, low volume); the name is user-editable afterwards.
//! Clicking a bookmark teleports through the normal geo_nav path, preserving
//! the camera mode. Bookmarks persist in the user store (`data/user.json`).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::GridCell;

use crate::platform::http;
use crate::resources::CameraState;
use crate::systems::egui_manager::EguiManager;
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE};
use crate::systems::geo_nav::{
    nominatim_base_url, world_to_lat_lng, CameraPose, GeoNavState, Teleport,
};
use crate::systems::map_stream::MapStream;
use crate::systems::user_store::UserStore;

/// Bookmarks UI state + the reverse-geocode result channel.
#[derive(Resource, Default)]
pub struct BookmarksState {
    pub panel_open: bool,
    /// Bookmark being renamed: (id, edit buffer).
    editing: Option<(u64, String)>,
    /// Finished reverse lookups: (bookmark id, address).
    resolved: Arc<Mutex<Vec<(u64, String)>>>,
}

impl BookmarksState {
    /// Resolve a bookmark's address on a background thread. The reverse lookup
    /// uses only lat/lng — altitude never enters it, so a bookmark taken from
    /// orbit resolves the same as one taken at street level. What it resolves to
    /// is the point on the globe directly under the camera; over open ocean (or
    /// on a network failure) Nominatim has no address, so we fall back to the
    /// coordinates rather than leaving the name stuck on the placeholder.
    pub fn request_address(&self, id: u64, lat: f64, lng: f64) {
        let resolved = self.resolved.clone();
        // Detached fetch on the IoTaskPool (ADR-033 seam) — no OS thread, so it
        // works on wasm; the `resolved` channel and its drain are unchanged.
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
                        json.get("display_name").and_then(|v| v.as_str()).map(short_address)
                    })
                    .unwrap_or_else(|| coord_label(lat, lng));
                if let Ok(mut g) = resolved.lock() {
                    g.push((id, name));
                }
            })
            .detach();
    }
}

/// First few segments of a Nominatim display_name — enough to recognise the
/// place without the province/country tail.
fn short_address(display_name: &str) -> String {
    let parts: Vec<&str> = display_name.split(',').map(str::trim).take(3).collect();
    parts.join(", ")
}

/// Fallback name when the reverse lookup finds no address (open ocean, or a
/// failed request): the signed coordinates as N/S, E/W.
fn coord_label(lat: f64, lng: f64) -> String {
    let ns = if lat >= 0.0 { 'N' } else { 'S' };
    let ew = if lng >= 0.0 { 'E' } else { 'W' };
    format!("{:.3}°{ns}, {:.3}°{ew}", lat.abs(), lng.abs())
}

/// Fold finished reverse lookups into the bookmark names.
pub fn poll_bookmark_addresses(
    bookmarks: Res<BookmarksState>,
    mut store: ResMut<UserStore>,
) {
    let resolved: Vec<(u64, String)> = bookmarks
        .resolved
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    for (id, name) in resolved {
        if let Some(b) = store.data.bookmarks.iter_mut().find(|b| b.id == id) {
            // Don't clobber a name the user already edited.
            if b.name == PLACEHOLDER_NAME {
                b.name = name;
                store.dirty = true;
            }
        }
    }
}

/// Name a bookmark carries until its address resolves.
pub const PLACEHOLDER_NAME: &str = "Resolving address…";

/// The Bookmarks side panel: click a name to jump there (camera view is
/// preserved), 🔄 to update it to the current view, ✏ to rename, 🗑 to delete.
#[allow(clippy::too_many_arguments)]
pub fn render_bookmarks_panel(
    mut egui: ResMut<EguiManager>,
    mut bookmarks: ResMut<BookmarksState>,
    mut store: ResMut<UserStore>,
    mut nav: ResMut<GeoNavState>,
    stream: Res<MapStream>,
    camera_state: Res<CameraState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
) {
    let open_before = right_panel.is(crate::resources::RightPanel::Places);
    let mut open = open_before;

    // Current location + view, for the per-bookmark "update to here" action.
    let current = camera.single().ok().zip(stream.geo_anchor()).map(
        |((cell, tf), anchor)| {
            let wx = cell.x as f64 * WORLD_CELL_EDGE as f64 + tf.translation.x as f64;
            let wz = cell.z as f64 * WORLD_CELL_EDGE as f64 + tf.translation.z as f64;
            let (lat, lng) = world_to_lat_lng(anchor, wx, wz);
            let alt = cell.y as f64 * WORLD_CELL_EDGE as f64 + tf.translation.y as f64;
            (lat, lng, alt, CameraPose::capture(&camera_state))
        },
    );

    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let ui_root = egui.ui_mut();
    let mut close = false;
    let mut delete: Option<u64> = None;
    let mut update: Option<u64> = None;
    let mut teleport: Option<Teleport> = None;
    let mut commit_rename: Option<(u64, String)> = None;

    crate::systems::egui_manager::responsive_right_panel(
        "bookmarks_panel",
        compact,
        viewport_w,
        280.0,
    )
    .show_collapsible(ui_root, &mut open, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Places");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("✕").clicked() {
                        close = true;
                    }
                });
            });
            ui.separator();
            if store.data.bookmarks.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("No bookmarks yet — click ☆ in the corner overlay to save the current location.")
                        .size(13.0)
                        .color(egui::Color32::from_gray(160)),
                );
            }
            let mut editing = bookmarks.editing.take();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for b in &store.data.bookmarks {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        match &mut editing {
                            // Rename in place; Enter or ✔ commits.
                            Some((id, buf)) if *id == b.id => {
                                let field = ui.add(
                                    egui::TextEdit::singleline(buf).desired_width(170.0),
                                );
                                let submit = ui.button("✔").clicked()
                                    || (field.lost_focus()
                                        && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                                if submit {
                                    commit_rename = Some((b.id, buf.clone()));
                                    editing = None;
                                }
                            }
                            _ => {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new(&b.name).size(13.0),
                                        )
                                        .wrap_mode(egui::TextWrapMode::Truncate)
                                        .min_size(egui::vec2(178.0, 0.0)),
                                    )
                                    .on_hover_text(format!(
                                        "{:.5}, {:.5}, {:.0} m — click to jump",
                                        b.lat, b.lng, b.altitude
                                    ))
                                    .clicked()
                                {
                                    // Keep the *current* camera mode and move it to
                                    // the bookmark's spot, rather than snapping back
                                    // to the mode the bookmark was saved in (`pose:
                                    // None` skips `CameraPose::apply`, so
                                    // `apply_teleport` repositions the mode you're in
                                    // — in Follow that means the avatar walks there).
                                    teleport = Some(Teleport {
                                        lat: b.lat,
                                        lng: b.lng,
                                        altitude: Some(b.altitude),
                                        pose: None,
                                    });
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("🗑").clicked() {
                                            delete = Some(b.id);
                                        }
                                        if ui.small_button("✏").clicked() {
                                            editing = Some((b.id, b.name.clone()));
                                        }
                                        if ui
                                            .add_enabled(current.is_some(), egui::Button::new("🔄").small())
                                            .on_hover_text("Update to current position and view")
                                            .clicked()
                                        {
                                            update = Some(b.id);
                                        }
                                    },
                                );
                            }
                        }
                    });
                }
            });
            bookmarks.editing = editing;
        });

    if let (Some(id), Some((lat, lng, alt, pose))) = (update, current) {
        // Keep the name; repoint the bookmark at where we are now.
        if let Some(b) = store.data.bookmarks.iter_mut().find(|b| b.id == id) {
            b.lat = lat;
            b.lng = lng;
            b.altitude = alt;
            b.pose = pose;
            store.dirty = true;
            tracing::info!(id, lat, lng, "updated bookmark to current view");
        }
    }

    if let Some((id, name)) = commit_rename {
        let name = name.trim().to_string();
        if !name.is_empty() {
            if let Some(b) = store.data.bookmarks.iter_mut().find(|b| b.id == id) {
                b.name = name;
                store.dirty = true;
            }
        }
    }
    if let Some(id) = delete {
        store.data.bookmarks.retain(|b| b.id != id);
        store.dirty = true;
    }
    if let Some(tp) = teleport {
        nav.pending_teleport = Some(tp);
    }
    if close || (open_before && !open) {
        *right_panel = crate::resources::RightPanel::None;
    }
}
