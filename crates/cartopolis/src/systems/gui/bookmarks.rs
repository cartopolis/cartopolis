//! Places: a side panel (top bar ▸ Places) holding saved locations in a
//! directory tree, and the machinery behind the HUD's ★ button.
//!
//! A new place is named by reverse-geocoding its coordinates through Nominatim
//! (same usage policy as the search bar: identifying User-Agent, background
//! thread, low volume); the name is user-editable afterwards. Clicking a place
//! teleports through the normal geo_nav path, preserving the camera mode.
//! Everything persists in the user store (`data/user.json`).
//!
//! **The tree.** Places live in nested folders ([`crate::systems::user_store::PlaceFolder`])
//! — an ordinary directory structure, dragged and dropped like one. A folder
//! carries a colour, and every place inside it is drawn in that colour on the
//! map (`crate::systems::place_pins`), which is what makes a group readable at a
//! glance instead of a list to scroll. The tree is flattened into [`Row`]s once
//! per frame and rendered as a
//! flat list with indentation: recursion through egui would need the store
//! borrowed mutably inside its own draw closure, and the rows are what the drop
//! targets are anyway.
//!
//! **Rows drag off their name, not off `Ui::dnd_drag_source`.** That helper
//! interacts over the whole row with `Sense::drag()` *only*, and egui marks a
//! drag-only widget as dragged the instant the pointer goes down — no movement
//! threshold, because with nothing else sensing the press there is nothing to
//! disambiguate ("this widget is just sensitive to drags, so we can mark it as
//! dragged right away", egui's `interaction.rs`). Every button in the row was
//! then inside a widget that began dragging on mouse-down, and the row's
//! contents were re-drawn into an `Order::Tooltip` layer, which always yields an
//! empty `Response` — so pressing ✏ started a drag and the click was lost.
//!
//! The drag therefore lives on a single widget, the row's name, declared
//! `click_and_drag`. Sensing both, egui postpones the decision until it knows,
//! so a click still clicks (jump, or fold the folder) and only a press-and-move
//! starts a drag. The payload goes through `Response::dnd_set_drag_payload`,
//! which is the alternative egui documents for this; it paints no ghost at the
//! cursor, so [`drag_ghost`] does.

use crate::components::FreeCamera;
use crate::resources::MapAnchor;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::GridCell;

use crate::grid::WORLD_CELL_EDGE;
use crate::platform::http;
use crate::resources::CameraState;
use crate::systems::egui_manager::EguiManager;
use crate::systems::geo_nav::{
    nominatim_base_url, world_to_lat_lng, CameraPose, GeoNavState, Teleport,
};
use crate::systems::theme;
use crate::systems::user_store::UserStore;

/// What a rename or a drag is targeting — the two id spaces are separate, so it
/// has to say which tree the id belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Place(u64),
    Folder(u64),
}

/// A rename in progress.
struct Rename {
    item: Item,
    buf: String,
    /// The field still has to be handed the keyboard — set when ✏ is clicked and
    /// cleared on the frame the field actually takes focus. Without it, ✏ opens a
    /// field you then have to click into before you can type.
    focus: bool,
}

/// Places UI state + the reverse-geocode result channel.
#[derive(Resource, Default)]
pub struct BookmarksState {
    pub panel_open: bool,
    /// Item being renamed, if any.
    editing: Option<Rename>,
    /// Finished reverse lookups: (bookmark id, address).
    resolved: Arc<Mutex<Vec<(u64, String)>>>,
    /// Thumbnails already uploaded, by photo stem. `None` means "looked and there
    /// was nothing there" — cached just as firmly as a hit, because the lookup
    /// runs once per row per frame and a miss that is not remembered is a file
    /// read sixty times a second for a photo that will never arrive.
    photo_textures: HashMap<String, Option<egui::TextureHandle>>,
    /// The capture in flight, and which place it is for. One at a time: the
    /// platform arms all put a modal in front of the user, so a second request
    /// could not be answered anyway.
    capture: Option<PendingPhoto>,
    /// The photo being looked at full-size, if any.
    viewing: Option<PhotoView>,
    /// Why the last photo failed, shown in the panel until the next attempt.
    photo_error: Option<String>,
}

/// A photo the user asked for and has not got yet.
struct PendingPhoto {
    place: u64,
    stage: PhotoStage,
}

/// The two waits between "take a photo" and a thumbnail in the row.
enum PhotoStage {
    /// The camera app, the file dialog or the browser's picker is up.
    Capturing(crate::platform::photo::Capture),
    /// The bytes are in and are being decoded, straightened and re-encoded.
    ///
    /// On the `IoTaskPool` rather than inline, because this is the one expensive
    /// step: a 12 MP phone photo is a few hundred milliseconds of decode and
    /// resample, and doing it in the system would drop that as a single stalled
    /// frame on exactly the frame the user comes back from the camera. (On wasm
    /// the pool is the main thread, so there it is deferred rather than moved.)
    Preparing(Arc<Mutex<Option<Result<crate::utils::photo::Prepared, String>>>>),
}

/// The full-size viewer over the panel.
struct PhotoView {
    place: u64,
    name: String,
    /// Photo stem, so the viewer closes itself if the photo is replaced or
    /// deleted underneath it.
    stem: String,
    /// Uploaded once the bytes are in hand.
    texture: Option<egui::TextureHandle>,
    /// Set when there is nothing left to wait for: no such photo, or one the
    /// decoder cannot read. Distinct from `texture: None`, which on the web means
    /// "the read has not come back yet" — collapsing the two is how a photo that
    /// is merely slow ends up reported as broken.
    unavailable: bool,
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
                        json.get("display_name")
                            .and_then(|v| v.as_str())
                            .map(short_address)
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
pub fn poll_bookmark_addresses(bookmarks: Res<BookmarksState>, mut store: ResMut<UserStore>) {
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

/// Points of indentation per tree level.
const INDENT: f32 = 12.0;

/// One line of the flattened tree. Owned copies of what the row draws, so the
/// render loop holds no borrow of the store and can queue mutations freely.
enum Row {
    Folder {
        id: u64,
        depth: usize,
        name: String,
        color: [f32; 3],
        collapsed: bool,
        /// Places in this folder at any depth — shown on a collapsed folder so a
        /// closed directory still says how much is in it.
        count: usize,
    },
    Place {
        id: u64,
        depth: usize,
        name: String,
        color: [f32; 3],
        lat: f64,
        lng: f64,
        altitude: f64,
        /// Stem of the place's photo, if it has one.
        photo: Option<String>,
    },
}

/// Flatten the directory tree into draw order: each folder, then its child
/// folders, then its places. A collapsed folder contributes only its own row.
fn flatten(store: &UserStore, parent: Option<u64>, depth: usize, out: &mut Vec<Row>) {
    // Guard against a cycle in a hand-edited/corrupted user.json: without it a
    // parent loop recurses until the stack runs out.
    if depth > 32 {
        return;
    }
    for f in store.data.folders.iter().filter(|f| f.parent == parent) {
        out.push(Row::Folder {
            id: f.id,
            depth,
            name: f.name.clone(),
            color: f.color,
            collapsed: f.collapsed,
            count: store
                .data
                .bookmarks
                .iter()
                .filter(|b| {
                    b.folder == Some(f.id)
                        || b.folder.is_some_and(|id| store.is_descendant(id, f.id))
                })
                .count(),
        });
        if !f.collapsed {
            flatten(store, Some(f.id), depth + 1, out);
        }
    }
    for b in store.data.bookmarks.iter().filter(|b| b.folder == parent) {
        out.push(Row::Place {
            id: b.id,
            depth,
            name: b.name.clone(),
            color: store.pin_color(b.folder),
            lat: b.lat,
            lng: b.lng,
            altitude: b.altitude,
            photo: b.photo.clone(),
        });
    }
}

/// Everything the panel wants done, collected while drawing (the store is
/// borrowed immutably there) and applied afterwards.
#[derive(Default)]
struct Actions {
    close: bool,
    new_folder: bool,
    delete_place: Option<u64>,
    delete_folder: Option<u64>,
    update: Option<u64>,
    teleport: Option<Teleport>,
    /// What to call the teleport's destination in the location history.
    teleport_label: Option<String>,
    /// "Route here" on a place: its name and where it is.
    route: Option<(String, f64, f64)>,
    rename: Option<(Item, String)>,
    recolor: Option<(u64, [f32; 3])>,
    toggle_collapse: Option<u64>,
    /// (what was dragged, which folder it was dropped on — `None` = top level).
    drop: Option<(Item, Option<u64>)>,
    /// Start a photo capture for this place.
    take_photo: Option<u64>,
    /// Forget this place's photo.
    clear_photo: Option<u64>,
    /// Open the full-size viewer: the place, its name and its photo stem.
    view_photo: Option<(u64, String, String)>,
    /// Abandon the capture in flight.
    cancel_photo: bool,
    /// Clear the last photo failure from the panel.
    dismiss_photo_error: bool,
}

fn egui_color(c: [f32; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(
        (c[0] * 255.0) as u8,
        (c[1] * 255.0) as u8,
        (c[2] * 255.0) as u8,
    )
}

/// The fold arrow, painted rather than written: ▶/▼ are not in the bundled icon
/// font and come out as tofu boxes next to every folder.
fn fold_arrow(ui: &mut egui::Ui, collapsed: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let c = rect.center();
        let r = 4.0;
        let color = if response.hovered() {
            ui.visuals().strong_text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        let points = if collapsed {
            vec![
                egui::pos2(c.x - r * 0.6, c.y - r),
                egui::pos2(c.x - r * 0.6, c.y + r),
                egui::pos2(c.x + r * 0.8, c.y),
            ]
        } else {
            vec![
                egui::pos2(c.x - r, c.y - r * 0.6),
                egui::pos2(c.x + r, c.y - r * 0.6),
                egui::pos2(c.x, c.y + r * 0.8),
            ]
        };
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            color,
            egui::Stroke::NONE,
        ));
    }
    response
}

/// Draw `item`'s row as a rename field, if that is the row being renamed, and
/// say whether it did — the caller then skips its normal row.
fn rename_row(
    ui: &mut egui::Ui,
    act: &mut Actions,
    editing: &mut Option<Rename>,
    item: Item,
    depth: usize,
) -> bool {
    let Some(edit) = editing.as_mut().filter(|e| e.item == item) else {
        return false;
    };
    let mut commit = false;
    ui.horizontal(|ui| {
        // Line the field up with the row's name, past the fold arrow / pin dot.
        ui.add_space(depth as f32 * INDENT + 12.0);
        commit = rename_field(ui, item, edit);
    });
    if commit {
        if let Some(edit) = editing.take() {
            act.rename = Some((item, edit.buf));
        }
    }
    true
}

/// The rename field. Given a stable id of its own — an auto-id would shift when
/// the row swaps its label for this field, and a field whose id changes between
/// frames loses focus on the frame it gains it. Returns true when the edit is
/// committed (Enter or ✔).
fn rename_field(ui: &mut egui::Ui, item: Item, edit: &mut Rename) -> bool {
    let id = match item {
        Item::Place(id) => egui::Id::new(("place_rename", id)),
        Item::Folder(id) => egui::Id::new(("folder_rename", id)),
    };
    let mut commit = false;
    // ✔ takes the right edge first, then the field is sized from what is
    // genuinely left. Reserving a guessed width for the button instead made the
    // row a few points wider than the panel — and a resizable `Panel` stores its
    // size from its content rect, so the panel grew by that much *every frame*
    // for as long as the field was open.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        commit = ui
            .button(theme::first_drawable(ui.ctx(), &["✔", "✓", "OK"]))
            .on_hover_text("Apply")
            .clicked();
        // `desired_width` is the field's *text* width; its frame padding and
        // stroke sit outside that, so they have to come off too.
        let margin = 2.0 * ui.spacing().button_padding.x + 4.0;
        let field = ui.add(
            egui::TextEdit::singleline(&mut edit.buf)
                .id(id)
                .desired_width((ui.available_width() - margin).max(40.0)),
        );
        if edit.focus {
            field.request_focus();
            edit.focus = false;
        }
        commit |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    });
    commit
}

/// The Places side panel: a directory tree of folders and saved locations.
/// Click a name to jump there (camera view is preserved), ⚑ to route to it, 🔄 to
/// update it to the current view, ✏ to rename, 🗑 to delete; drag a row onto a
/// folder to move it, and use the folder's colour swatch to recolour its pins on
/// the map.
///
/// A place's own actions are in its context menu (long-press on a phone, right-click
/// on a desktop) rather than in the row — see [`place_row`] for why.
#[allow(clippy::too_many_arguments)]
pub fn render_bookmarks_panel(
    mut egui: ResMut<EguiManager>,
    mut bookmarks: ResMut<BookmarksState>,
    mut store: ResMut<UserStore>,
    mut nav: ResMut<GeoNavState>,
    camera_state: Res<CameraState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    map_anchor: Res<MapAnchor>,
) {
    let open_before = right_panel.is(crate::resources::RightPanel::Places);
    // Drawn before the early return below, so a photo opened from the panel
    // survives the panel being closed behind it. It is a window rather than
    // something inside the panel because on a phone the panel *is* the screen and
    // a photo shown inside a 300 pt column is a thumbnail with extra steps.
    photo_viewer(&egui.ctx, egui.viewport_pts(), &mut bookmarks);
    // Nothing below this line does anything a closed panel can observe — `open`
    // cannot become true without the panel already being open, and the closure that
    // sets `act` never runs — but `flatten` is not free: it clones a `String` per
    // node and, for every folder, walks all bookmarks calling `is_descendant`. That
    // is O(folders × places × depth) plus an allocation per row, every frame, for a
    // panel nobody is looking at. `render_route_panel` guards itself the same way.
    if !open_before {
        return;
    }
    let mut open = open_before;

    // Current location + view, for the per-place "update to here" action.
    let current = camera
        .single()
        .ok()
        .zip(map_anchor.geo)
        .map(|((cell, tf), anchor)| {
            let wx = cell.x as f64 * WORLD_CELL_EDGE as f64 + tf.translation.x as f64;
            let wz = cell.z as f64 * WORLD_CELL_EDGE as f64 + tf.translation.z as f64;
            let (lat, lng) = world_to_lat_lng(anchor, wx, wz);
            let alt = cell.y as f64 * WORLD_CELL_EDGE as f64 + tf.translation.y as f64;
            (lat, lng, alt, CameraPose::capture(&camera_state))
        });

    let mut rows = Vec::new();
    flatten(&store, None, 0, &mut rows);

    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let ui_root = egui.ui_mut();
    let mut act = Actions::default();
    let mut editing = bookmarks.editing.take();
    // Taken out and put back for the same reason `editing` is: the draw closure
    // cannot hold a borrow of the resource it is being called from.
    let mut thumbs = std::mem::take(&mut bookmarks.photo_textures);
    let capturing_for = bookmarks.capture.as_ref().map(|c| c.place);
    let photo_error = bookmarks.photo_error.clone();

    crate::systems::egui_manager::responsive_right_panel(
        "bookmarks_panel",
        compact,
        viewport_w,
        300.0,
    )
    .show_collapsible(ui_root, &mut open, |ui| {
        let panel = crate::resources::RightPanel::Places;
        act.close =
            crate::systems::theme::panel_header_with(ui, panel.icons(), panel.label(), |ui| {
                // Spelled out rather than an icon: 📁 is not in the bundled icon
                // font and renders as a tofu box.
                if ui
                    .button("+ Folder")
                    .on_hover_text("New folder — drag places into it to group them")
                    .clicked()
                {
                    act.new_folder = true;
                }
            });
        // A capture puts the camera (or a file dialog) in front of everything, so
        // this strip is only seen on the way back — which is exactly when "did
        // that work?" is the question. It is also the only place the capture can
        // be abandoned: on Android nothing tells us the user pressed Back.
        if let Some(place) = capturing_for {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spinner();
                let waiting = rows
                    .iter()
                    .find_map(|row| match row {
                        Row::Place { id, name, .. } if *id == place => Some(name.as_str()),
                        _ => None,
                    })
                    .unwrap_or("this place");
                ui.label(
                    egui::RichText::new(format!("Waiting for a photo for {waiting}…"))
                        .size(12.0)
                        .color(theme::TEXT_DIM),
                );
                if ui.small_button("Cancel").clicked() {
                    act.cancel_photo = true;
                }
            });
        }
        if let Some(message) = &photo_error {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(message.as_str())
                        .size(12.0)
                        .color(theme::WARN),
                );
                if ui.small_button("Dismiss").clicked() {
                    act.dismiss_photo_error = true;
                }
            });
        }
        if rows.is_empty() {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(
                    "No places yet — click ☆ in the corner overlay to save the current \
                     location, then 📁+ to sort them into folders.",
                )
                .size(13.0)
                .color(theme::TEXT_DIM),
            );
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for row in &rows {
                ui.add_space(2.0);
                match row {
                    Row::Folder {
                        id,
                        depth,
                        name,
                        color,
                        collapsed,
                        count,
                    } => {
                        folder_row(
                            ui,
                            &mut act,
                            &mut editing,
                            *id,
                            *depth,
                            name,
                            *color,
                            *collapsed,
                            *count,
                        );
                    }
                    Row::Place {
                        id,
                        depth,
                        name,
                        color,
                        lat,
                        lng,
                        altitude,
                        photo,
                    } => {
                        place_row(
                            ui,
                            &mut act,
                            &mut editing,
                            &mut thumbs,
                            *id,
                            *depth,
                            name,
                            *color,
                            (*lat, *lng, *altitude),
                            photo.as_deref(),
                            current.is_some(),
                        );
                    }
                }
            }
            // Somewhere to drop a row to get it *out* of a folder. Only while a
            // drag is live — an always-present strip is just clutter, and the
            // tree has no other empty space to aim at once it fills the panel.
            if egui::DragAndDrop::has_payload_of_type::<Item>(ui.ctx()) {
                ui.add_space(6.0);
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 26.0),
                    egui::Sense::hover(),
                );
                let hot = response.contains_pointer();
                let stroke = egui::Stroke::new(1.0, if hot { theme::ACCENT } else { theme::LINE });
                ui.painter()
                    .rect_stroke(rect.shrink(1.0), 4.0, stroke, egui::StrokeKind::Inside);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "drop here for the top level",
                    egui::FontId::proportional(12.0),
                    stroke.color,
                );
                if let Some(item) = response.dnd_release_payload::<Item>() {
                    act.drop = Some((*item, None));
                }
            }
        });
    });
    bookmarks.editing = editing;
    bookmarks.photo_textures = thumbs;
    drag_ghost(&egui.ctx, &rows);

    let close = act.close;
    apply_photo_actions(&mut bookmarks, &mut store, &act);
    apply(&mut store, &mut nav, act.update.zip(current), act);

    // ✕, or the panel dragged/collapsed shut by its own resize handle.
    if close || (open_before && !open) {
        *right_panel = crate::resources::RightPanel::None;
    }
}

/// One folder row: fold arrow, colour swatch (the pin colour of everything in
/// it), name, and the rename/delete actions. The whole row is both a drag source
/// and a drop target, which is what makes the tree a directory structure.
#[allow(clippy::too_many_arguments)]
fn folder_row(
    ui: &mut egui::Ui,
    act: &mut Actions,
    editing: &mut Option<Rename>,
    id: u64,
    depth: usize,
    name: &str,
    color: [f32; 3],
    collapsed: bool,
    count: usize,
) {
    if rename_row(ui, act, editing, Item::Folder(id), depth) {
        return;
    }

    let row = ui
        .horizontal(|ui| {
            ui.add_space(depth as f32 * INDENT);
            if fold_arrow(ui, collapsed).clicked() {
                act.toggle_collapse = Some(id);
            }
            let mut rgb = [
                (color[0] * 255.0) as u8,
                (color[1] * 255.0) as u8,
                (color[2] * 255.0) as u8,
            ];
            // A compact square swatch: the default colour button is as wide as a
            // text field and would crowd the name out of a 300 pt panel.
            ui.spacing_mut().interact_size.x = 22.0;
            if ui
                .color_edit_button_srgb(&mut rgb)
                .on_hover_text("Pin colour for every place in this folder")
                .changed()
            {
                act.recolor = Some((
                    id,
                    [
                        rgb[0] as f32 / 255.0,
                        rgb[1] as f32 / 255.0,
                        rgb[2] as f32 / 255.0,
                    ],
                ));
            }
            // Buttons take the right edge first; the name then gets exactly what
            // is left, so a long folder name truncates instead of running under
            // them.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::action_button(ui, "🗑")
                    .on_hover_text("Delete the folder (its places move up a level)")
                    .clicked()
                {
                    act.delete_folder = Some(id);
                }
                let rename_icon = theme::first_drawable(ui.ctx(), &["✏", "✎", "Aa"]);
                if theme::action_button(ui, rename_icon)
                    .on_hover_text("Rename")
                    .clicked()
                {
                    *editing = Some(Rename {
                        item: Item::Folder(id),
                        buf: name.to_string(),
                        focus: true,
                    });
                }
                let label = if collapsed && count > 0 {
                    format!("{name}  ({count})")
                } else {
                    name.to_string()
                };
                fill_remaining(ui, |ui| {
                    // The name is the drag handle (see `drag_handle`). Clicking it
                    // folds, which is what a tree row's name does everywhere else.
                    let name = ui.add(
                        egui::Label::new(
                            egui::RichText::new(label)
                                .size(13.5)
                                .strong()
                                .color(egui_color(color)),
                        )
                        .truncate()
                        .sense(egui::Sense::click_and_drag()),
                    );
                    name.dnd_set_drag_payload(Item::Folder(id));
                    if name.clicked() {
                        act.toggle_collapse = Some(id);
                    }
                });
            });
        })
        .response;

    // Drop target: anything released over the row lands in this folder.
    if row.dnd_hover_payload::<Item>().is_some() {
        ui.painter().rect_stroke(
            row.rect.expand(1.0),
            4.0,
            egui::Stroke::new(1.5, egui_color(color)),
            egui::StrokeKind::Inside,
        );
    }
    if let Some(item) = row.dnd_release_payload::<Item>() {
        act.drop = Some((*item, Some(id)));
    }
}

/// One place row: its photo (or the coloured pin dot matching its map pin), the
/// name — click to jump, long-press or right-click for everything else.
///
/// **The actions are in a context menu and not in the row, and that is a phone
/// decision.** The row used to end in four small buttons (route, update, rename,
/// delete). On a 300 pt panel that left about half the width for the name, which
/// is the one thing in the row anybody reads; on a phone, where the panel is the
/// whole screen and the buttons are still finger-sized, it left a name column too
/// narrow for "Willem Alexander Sportpark" to be distinguishable from the next
/// one. Moving them into [`egui::Response::context_menu`] gives the width back
/// and costs nothing on the desktop, where right-click is where a list row's
/// actions live anyway.
///
/// egui reaches that menu from a touch screen on its own: `Response::secondary_clicked`
/// is true for a *long touch* as well as a right-click, and this client feeds egui
/// real `Event::Touch`es (`egui_manager::pointer_events`), which is what makes
/// `InputState::any_touches` — the condition on the whole long-press path — true
/// in the first place.
#[allow(clippy::too_many_arguments)]
fn place_row(
    ui: &mut egui::Ui,
    act: &mut Actions,
    editing: &mut Option<Rename>,
    thumbs: &mut HashMap<String, Option<egui::TextureHandle>>,
    id: u64,
    depth: usize,
    name: &str,
    color: [f32; 3],
    (lat, lng, altitude): (f64, f64, f64),
    photo: Option<&str>,
    can_update: bool,
) {
    // Not a drag source while renaming — see `folder_row`.
    if rename_row(ui, act, editing, Item::Place(id), depth) {
        return;
    }

    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * INDENT);
        match photo.and_then(|stem| thumbnail(ui, thumbs, stem, color).map(|t| (stem, t))) {
            Some((stem, tile)) => {
                if tile.clicked() {
                    act.view_photo = Some((id, name.to_string(), stem.to_string()));
                }
            }
            // No photo (or its file is gone): the dot, which is the map pin's
            // colour, so the panel and the map read as one thing.
            None => {
                ui.label(
                    egui::RichText::new(theme::first_drawable(ui.ctx(), &["📍", "⌖", "•"]))
                        .size(12.0)
                        .color(egui_color(color)),
                );
            }
        }
        let title = name.to_string();
        // The name is the drag handle (see `drag_handle`): sensing both click and
        // drag, egui waits to see whether the press turns into a drag, so a click
        // still jumps.
        let name_response = ui
            .add(
                egui::Button::new(egui::RichText::new(name).size(13.0))
                    .wrap_mode(egui::TextWrapMode::Truncate)
                    .min_size(egui::vec2(ui.available_width(), 0.0))
                    .sense(egui::Sense::click_and_drag()),
            )
            .on_hover_text(format!(
                "{lat:.5}, {lng:.5}, {altitude:.0} m — click to jump, \
                 long-press for actions, drag into a folder"
            ));
        name_response.dnd_set_drag_payload(Item::Place(id));

        // A long touch sets `clicked()` *as well as* `secondary_clicked()` —
        // egui's press-and-hold handler synthesises the click so that a widget
        // with no menu still responds to the press (`interaction.rs`). Without
        // this guard, opening the menu on a phone would also fly the camera to
        // the place behind it, which is exactly the view the menu is covering.
        if name_response.clicked() && !name_response.long_touched() {
            // Keep the *current* camera mode and move it to the place's spot,
            // rather than snapping back to the mode it was saved in (`pose: None`
            // skips `CameraPose::apply`, so `apply_teleport` repositions the mode
            // you're in — in Follow that means the avatar walks there).
            act.teleport = Some(Teleport {
                lat,
                lng,
                altitude: Some(altitude),
                pose: None,
            });
            act.teleport_label = Some(title.clone());
        }

        name_response.context_menu(|ui| {
            place_menu(ui, act, editing, id, &title, (lat, lng), photo, can_update);
        });
    });
}

/// The place row's actions. Ordered by how often they are wanted, with the
/// destructive one last and separated.
#[allow(clippy::too_many_arguments)]
fn place_menu(
    ui: &mut egui::Ui,
    act: &mut Actions,
    editing: &mut Option<Rename>,
    id: u64,
    name: &str,
    (lat, lng): (f64, f64),
    photo: Option<&str>,
    can_update: bool,
) {
    // Wide enough that "Update to the current view" fits on one line; egui would
    // otherwise size the menu to the longest label and wrap the rest.
    ui.set_min_width(190.0);

    // A saved place is the destination that needs no typing, which is what makes
    // it the important one on a phone. It wears the Route panel's own glyph
    // (probed, never guessed — `theme::first_drawable`) so the entry and the panel
    // it opens read as one thing.
    let flag = theme::first_drawable(ui.ctx(), crate::resources::RightPanel::Route.icons());
    if ui.button(format!("{flag}  Route here")).clicked() {
        act.route = Some((name.to_string(), lat, lng));
        ui.close();
    }

    if crate::platform::photo::is_available() {
        let label = if photo.is_some() {
            "Replace the photo…"
        } else {
            crate::platform::photo::action_label()
        };
        // No glyph: 📷 is not in the bundled icon font and paints as a tofu box
        // (`theme::tests::no_painted_glyph_is_tofu` catches it), and there is no
        // near-enough substitute that is — so the entry is words, which a menu
        // can afford in a way a 20 pt button could not.
        if ui.button(label).clicked() {
            act.take_photo = Some(id);
            ui.close();
        }
    }
    if let Some(stem) = photo {
        if ui.button("View the photo").clicked() {
            act.view_photo = Some((id, name.to_string(), stem.to_string()));
            ui.close();
        }
        if ui.button("Remove the photo").clicked() {
            act.clear_photo = Some(id);
            ui.close();
        }
    }

    ui.separator();
    if ui
        .add_enabled(
            can_update,
            egui::Button::new(format!(
                "{}  Update to the current view",
                theme::first_drawable(ui.ctx(), &["🔄", "↻", "·"])
            )),
        )
        .clicked()
    {
        act.update = Some(id);
        ui.close();
    }
    if ui
        .button(format!(
            "{}  Rename",
            theme::first_drawable(ui.ctx(), &["✏", "✎", "Aa"])
        ))
        .clicked()
    {
        *editing = Some(Rename {
            item: Item::Place(id),
            buf: name.to_string(),
            focus: true,
        });
        ui.close();
    }
    ui.separator();
    if ui.button("🗑  Delete this place").clicked() {
        act.delete_place = Some(id);
        ui.close();
    }
}

/// Draw the photo tile at the head of a row, uploading it on first sight.
///
/// `None` when the place has no usable photo — which covers both "never had one"
/// and "the file is gone", and the caller draws the pin dot for either. A photo
/// whose files were deleted underneath the store (a wiped app-data directory, a
/// user.json copied to another machine without the `photos/` beside it) must
/// degrade to a place that simply has no picture, not to a broken tile.
fn thumbnail(
    ui: &mut egui::Ui,
    thumbs: &mut HashMap<String, Option<egui::TextureHandle>>,
    stem: &str,
    color: [f32; 3],
) -> Option<egui::Response> {
    // Both the read and the decode happen once per photo per run, and the
    // `None` is remembered too — see the field's own note.
    let texture = thumbs
        .entry(stem.to_string())
        .or_insert_with(|| {
            let bytes = crate::utils::photo_store::read_thumb(stem)?;
            let (pixels, size) = crate::utils::photo::decode_rgba(&bytes)
                .map_err(|e| tracing::warn!(stem, error = %e, "a place's thumbnail did not decode"))
                .ok()?;
            let image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
            Some(ui.ctx().load_texture(
                format!("place_thumb_{stem}"),
                image,
                egui::TextureOptions::LINEAR,
            ))
        })
        .as_ref()?;

    let side = crate::utils::photo::THUMB_POINTS;
    let response = ui.add(
        egui::Image::new(egui::load::SizedTexture::new(
            texture.id(),
            egui::vec2(side, side),
        ))
        .corner_radius(4.0)
        .sense(egui::Sense::click()),
    );
    // The photo takes the dot's place, so the folder colour it was carrying moves
    // to a ring around the tile — without it, giving a place a photo silently
    // removes it from its group as far as the eye is concerned.
    ui.painter().rect_stroke(
        response.rect,
        4.0,
        egui::Stroke::new(1.5, egui_color(color)),
        egui::StrokeKind::Outside,
    );
    Some(response.on_hover_text("Open the photo"))
}

/// Follow the pointer with the name of whatever is being dragged. `dnd_drag_source`
/// used to paint the row itself at the cursor; dragging off the name gives that
/// up, and without any ghost a drag reads as "nothing happened" until the
/// pointer reaches a folder that highlights.
fn drag_ghost(ctx: &egui::Context, rows: &[Row]) {
    let Some(item) = egui::DragAndDrop::payload::<Item>(ctx) else {
        return;
    };
    let Some(pos) = ctx.pointer_interact_pos() else {
        return;
    };
    let Some((name, color)) = rows.iter().find_map(|row| match (row, *item) {
        (
            Row::Folder {
                id, name, color, ..
            },
            Item::Folder(dragged),
        ) if *id == dragged => Some((name.as_str(), *color)),
        (
            Row::Place {
                id, name, color, ..
            },
            Item::Place(dragged),
        ) if *id == dragged => Some((name.as_str(), *color)),
        _ => None,
    }) else {
        return;
    };

    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("place_drag_ghost"),
    ));
    let galley = painter.layout_no_wrap(
        name.to_string(),
        egui::FontId::proportional(13.0),
        theme::CREAM,
    );
    let pad = egui::vec2(6.0, 3.0);
    // Offset off the cursor so it never covers the row being dropped onto.
    let rect = egui::Rect::from_min_size(pos + egui::vec2(12.0, 8.0), galley.size() + pad * 2.0);
    painter.rect(
        rect,
        4.0,
        theme::LABEL_CHIP,
        egui::Stroke::new(1.0, egui_color(color)),
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.min + pad, galley, theme::CREAM);
}

/// Lay `contents` out left-to-right across everything the row has left, from
/// inside a right-to-left button strip. Without it the name would be pushed up
/// against the buttons on the right instead of starting where the row does.
fn fill_remaining(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::left_to_right(egui::Align::Center),
        contents,
    );
}

/// Apply the frame's queued mutations to the store.
fn apply(
    store: &mut UserStore,
    nav: &mut GeoNavState,
    update: Option<(u64, (f64, f64, f64, CameraPose))>,
    act: Actions,
) {
    if act.new_folder {
        store.add_folder("New folder".to_string(), None);
    }
    if let Some((id, (lat, lng, alt, pose))) = update {
        // Keep the name; repoint the place at where we are now.
        if let Some(b) = store.data.bookmarks.iter_mut().find(|b| b.id == id) {
            b.lat = lat;
            b.lng = lng;
            b.altitude = alt;
            b.pose = pose;
            store.dirty = true;
            tracing::info!(id, lat, lng, "updated place to current view");
        }
    }
    if let Some((item, name)) = act.rename {
        let name = name.trim().to_string();
        if !name.is_empty() {
            match item {
                Item::Place(id) => {
                    if let Some(b) = store.data.bookmarks.iter_mut().find(|b| b.id == id) {
                        b.name = name;
                        store.dirty = true;
                    }
                }
                Item::Folder(id) => {
                    if let Some(f) = store.folder_mut(id) {
                        f.name = name;
                        store.dirty = true;
                    }
                }
            }
        }
    }
    if let Some((id, color)) = act.recolor {
        if let Some(f) = store.folder_mut(id) {
            f.color = color;
            store.dirty = true;
        }
    }
    if let Some(id) = act.toggle_collapse {
        if let Some(f) = store.folder_mut(id) {
            f.collapsed = !f.collapsed;
            store.dirty = true;
        }
    }
    if let Some((item, target)) = act.drop {
        match item {
            Item::Place(id) => store.move_place(id, target),
            Item::Folder(id) => {
                if !store.move_folder(id, target) {
                    tracing::debug!(id, ?target, "refused a folder drop that would cycle");
                }
            }
        }
    }
    if let Some(id) = act.delete_place {
        // Through the store rather than by retaining in place: deleting a place
        // has to take its photo files with it (`UserStore::remove_bookmark`).
        store.remove_bookmark(id);
    }
    if let Some(id) = act.delete_folder {
        store.remove_folder(id);
    }
    if let Some(tp) = act.teleport {
        // Name the visit, so it lists as itself in the address bar's recents
        // rather than as five decimals.
        nav.pending_label = act.teleport_label;
        nav.pending_teleport = Some(tp);
    }
    if let Some((name, lat, lng)) = act.route {
        // Queued rather than applied: this panel does not own `RouteState`, and
        // `geo_nav::drive_geocode` spends the request (see `geo_nav::RouteRequest`).
        nav.route_to(name, lat, lng);
    }
}

/// Apply the photo actions the panel collected.
///
/// Separate from [`apply`] because these are the only actions that touch
/// [`BookmarksState`] as well as the store — a capture is UI state that outlives
/// the frame that started it.
fn apply_photo_actions(bookmarks: &mut BookmarksState, store: &mut UserStore, act: &Actions) {
    if act.dismiss_photo_error {
        bookmarks.photo_error = None;
    }
    if act.cancel_photo {
        abandon_capture(bookmarks);
    }
    if let Some(place) = act.take_photo {
        // Only one capture at a time (see `BookmarksState::capture`), so asking
        // for a second photo abandons the first rather than queueing behind it.
        abandon_capture(bookmarks);
        bookmarks.photo_error = None;
        match crate::platform::photo::start() {
            Ok(capture) => {
                bookmarks.capture = Some(PendingPhoto {
                    place,
                    stage: PhotoStage::Capturing(capture),
                });
            }
            Err(message) => {
                tracing::warn!(error = %message, "could not start a photo capture");
                bookmarks.photo_error = Some(message);
            }
        }
    }
    if let Some(id) = act.clear_photo {
        forget_photo_textures(bookmarks, store, id);
        store.clear_bookmark_photo(id);
    }
    if let Some(id) = act.delete_place {
        // `apply` deletes the files; this drops the uploaded texture that was
        // pointing at them, which would otherwise sit in the cache for the rest
        // of the run under a stem nothing refers to.
        forget_photo_textures(bookmarks, store, id);
    }
    if let Some((place, name, stem)) = &act.view_photo {
        bookmarks.viewing = Some(PhotoView {
            place: *place,
            name: name.clone(),
            stem: stem.clone(),
            texture: None,
            unavailable: false,
        });
    }
}

/// Drop the cached thumbnail of a place's photo, and close the viewer if it is
/// the one being looked at.
fn forget_photo_textures(bookmarks: &mut BookmarksState, store: &UserStore, place: u64) {
    let Some(stem) = store
        .data
        .bookmarks
        .iter()
        .find(|b| b.id == place)
        .and_then(|b| b.photo.clone())
    else {
        return;
    };
    bookmarks.photo_textures.remove(&stem);
    if bookmarks.viewing.as_ref().is_some_and(|v| v.stem == stem) {
        bookmarks.viewing = None;
    }
}

/// End the capture in flight, telling the platform to release whatever it holds
/// for it (on Android, the empty MediaStore row the camera was given).
fn abandon_capture(bookmarks: &mut BookmarksState) {
    if let Some(mut pending) = bookmarks.capture.take() {
        if let PhotoStage::Capturing(capture) = &mut pending.stage {
            capture.cancel();
        }
    }
}

/// Drive the capture in flight to its end.
///
/// Registered on its own rather than inside the panel, and deliberately *not*
/// gated on the panel being open: the user is looking at a camera app while this
/// runs, may well come back to a closed panel, and a photo that only lands if the
/// right panel happens to be open would be a bug nobody could describe.
pub fn poll_place_photos(mut bookmarks: ResMut<BookmarksState>, mut store: ResMut<UserStore>) {
    let Some(pending) = bookmarks.capture.as_mut() else {
        return;
    };
    let place = pending.place;

    match &mut pending.stage {
        PhotoStage::Capturing(capture) => match capture.poll() {
            crate::platform::photo::Outcome::Pending => {}
            crate::platform::photo::Outcome::Cancelled => {
                bookmarks.capture = None;
            }
            crate::platform::photo::Outcome::Failed(message) => {
                tracing::warn!(error = %message, "photo capture failed");
                bookmarks.capture = None;
                bookmarks.photo_error = Some(message);
            }
            crate::platform::photo::Outcome::Ready(bytes) => {
                let slot = Arc::new(Mutex::new(None));
                let sink = slot.clone();
                IoTaskPool::get()
                    .spawn(async move {
                        let prepared = crate::utils::photo::prepare(&bytes);
                        if let Ok(mut sink) = sink.lock() {
                            *sink = Some(prepared);
                        }
                    })
                    .detach();
                pending.stage = PhotoStage::Preparing(slot);
            }
        },
        PhotoStage::Preparing(slot) => {
            let Some(prepared) = slot.lock().ok().and_then(|mut slot| slot.take()) else {
                return;
            };
            bookmarks.capture = None;
            match prepared.and_then(|prepared| store.set_bookmark_photo(place, &prepared)) {
                Ok(()) => tracing::info!(place, "attached a photo to a place"),
                Err(message) => {
                    tracing::warn!(place, error = %message, "could not attach the photo");
                    bookmarks.photo_error = Some(message);
                }
            }
        }
    }
}

/// The full-size photo, over everything.
///
/// Sized against the viewport rather than the image: a 1600 px photo in a window
/// that grew to fit it would be wider than a phone screen, and egui's `Image` will
/// letterbox into whatever it is given while keeping the aspect ratio.
fn photo_viewer(ctx: &egui::Context, viewport: egui::Vec2, bookmarks: &mut BookmarksState) {
    let Some(view) = bookmarks.viewing.as_mut() else {
        return;
    };
    // Asked for each frame until it resolves, rather than read once on open. The
    // row already had the thumbnail in hand, but the full photo is the one thing
    // the web build does *not* preload — it is megabytes and only wanted when
    // someone opens it — so the first ask starts an IndexedDB read and answers
    // `Loading`. Natively the first ask is the file read and this settles on the
    // opening frame.
    if view.texture.is_none() && !view.unavailable {
        match crate::utils::photo_store::read_full(&view.stem) {
            crate::platform::storage::Cached::Ready(bytes) => {
                match photo_texture(ctx, &view.stem, &bytes) {
                    Some(texture) => view.texture = Some(texture),
                    None => view.unavailable = true,
                }
            }
            crate::platform::storage::Cached::Loading => {}
            // Nothing under that key: the photo was there when the row drew a
            // thumbnail from it and is not now.
            crate::platform::storage::Cached::Missing => view.unavailable = true,
        }
    }

    let mut open = true;
    egui::Window::new(&view.name)
        .id(egui::Id::new(("place_photo", view.place)))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .max_size(viewport * 0.9)
        .default_size(viewport * 0.8)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| match (&view.texture, view.unavailable) {
            (Some(texture), _) => {
                ui.add(
                    egui::Image::new(egui::load::SizedTexture::new(
                        texture.id(),
                        texture.size_vec2(),
                    ))
                    .max_size(ui.available_size())
                    .maintain_aspect_ratio(true)
                    .corner_radius(4.0),
                );
            }
            (None, false) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new("Loading the photo…")
                            .size(13.0)
                            .color(theme::TEXT_DIM),
                    );
                });
            }
            (None, true) => {
                ui.label(
                    egui::RichText::new("This photo could not be opened.")
                        .size(13.0)
                        .color(theme::WARN),
                );
            }
        });
    if !open {
        bookmarks.viewing = None;
    }
}

/// Decode stored photo bytes and upload them as a texture.
fn photo_texture(ctx: &egui::Context, stem: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let (pixels, size) = crate::utils::photo::decode_rgba(bytes)
        .map_err(|e| tracing::warn!(stem, error = %e, "a place's photo did not decode"))
        .ok()?;
    let image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
    Some(ctx.load_texture(
        format!("place_photo_{stem}"),
        image,
        egui::TextureOptions::LINEAR,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive `place_row` through egui for a few frames, clicking or dragging at
    /// the row's name. Returns the actions the last frame produced and whether a
    /// drag payload was ever left behind by a plain click.
    fn drive(drag: bool) -> (Actions, bool) {
        let ctx = egui::Context::default();
        // The shipped theme, not `Visuals::dark()`: these tests are about
        // widths and hit-testing, both of which are padding-dependent.
        theme::install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));

        let mut target = egui::Pos2::ZERO;
        let mut out = Actions::default();
        let mut dragged = false;

        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        for frame in 0..5 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            match frame {
                1 => raw.events.push(egui::Event::PointerMoved(target)),
                2 => raw.events.push(button(target, true)),
                // Either release where we pressed (a click), or drag well clear
                // of the press origin first (egui only calls it a drag once the
                // pointer has moved too far to still be a click).
                3 if drag => raw
                    .events
                    .push(egui::Event::PointerMoved(target + egui::vec2(0.0, 80.0))),
                3 => raw.events.push(button(target, false)),
                _ => {}
            }

            let mut act = Actions::default();
            let mut editing = None;
            let mut thumbs = HashMap::new();
            let _ = ctx.run_ui(raw, |ui| {
                egui::Panel::right("places_test")
                    .default_size(300.0)
                    .show(ui, |ui| {
                        let top = ui.cursor().min;
                        place_row(
                            ui,
                            &mut act,
                            &mut editing,
                            &mut thumbs,
                            1,
                            0,
                            "Test place",
                            [1.0, 0.2, 0.2],
                            (1.0, 2.0, 10.0),
                            None,
                            true,
                        );
                        // The name button fills the row between the pin dot and
                        // the action buttons — aim well inside it.
                        target = egui::pos2(top.x + 70.0, (top.y + ui.cursor().min.y) * 0.5);
                    });
            });
            if act.teleport.is_some() {
                out.teleport = act.teleport;
            }
            if egui::DragAndDrop::has_payload_of_type::<Item>(&ctx) {
                dragged = true;
            }
        }
        (out, dragged)
    }

    /// Open a place row's context menu — with a right-click, or with a long touch
    /// — then click a point `dy` below the press, and return what the row asked
    /// for over the whole run.
    ///
    /// The menu is positioned at the pointer (`Popup::at_pointer_fixed`), so its
    /// items run down and to the right of wherever the press landed. Swept rather
    /// than aimed, for the same reason the old button-strip test swept in from the
    /// right edge: the item heights come from the font.
    fn click_place_menu(long_touch: bool, dy: f32) -> Actions {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
        let button = |pos, pressed, secondary: bool| egui::Event::PointerButton {
            pos,
            button: if secondary {
                egui::PointerButton::Secondary
            } else {
                egui::PointerButton::Primary
            },
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        let mut name = egui::Pos2::ZERO;
        let mut out = Actions::default();
        for frame in 0..14 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            // Real time, because a long press is defined in seconds; a 0.2 s frame
            // clears egui's threshold within the hold below.
            raw.time = Some(frame as f64 * 0.2);
            raw.predicted_dt = 0.2;
            let item = name + egui::vec2(30.0, dy);
            match (long_touch, frame) {
                // A right-click opens the menu on the release.
                (false, 1) => raw.events.push(egui::Event::PointerMoved(name)),
                (false, 2) => raw.events.push(button(name, true, true)),
                (false, 3) => raw.events.push(button(name, false, true)),
                // A long touch opens it *while the finger is still down*, and the
                // finger must stay down past egui's `max_click_duration` (0.6 s)
                // to get there — `is_long_press` also requires the button to be
                // down on the frame it fires, so a release that lands on the same
                // frame as the threshold cancels the whole thing. Hence 1.2 s of
                // hold before the lift at frame 7. The `Event::Touch`
                // is what makes it a touch at all: `InputState::any_touches` gates
                // the whole press-and-hold path, and only that event sets it.
                (true, 1) => {
                    raw.events.push(egui::Event::Touch {
                        device_id: egui::TouchDeviceId(0),
                        id: egui::TouchId(0),
                        phase: egui::TouchPhase::Start,
                        pos: name,
                        force: None,
                    });
                    raw.events.push(egui::Event::PointerMoved(name));
                    raw.events.push(button(name, true, false));
                }
                (true, 7) => {
                    raw.events.push(egui::Event::Touch {
                        device_id: egui::TouchDeviceId(0),
                        id: egui::TouchId(0),
                        phase: egui::TouchPhase::End,
                        pos: name,
                        force: None,
                    });
                    raw.events.push(button(name, false, false));
                }
                // The menu is up from here on: move onto an item and click it.
                (_, 10) => raw.events.push(egui::Event::PointerMoved(item)),
                (_, 11) => raw.events.push(button(item, true, false)),
                (_, 12) => raw.events.push(button(item, false, false)),
                _ => {}
            }

            let mut act = Actions::default();
            let mut editing = None;
            let mut thumbs = HashMap::new();
            let _ = ctx.run_ui(raw, |ui| {
                egui::Panel::right("places_test")
                    .default_size(300.0)
                    .show(ui, |ui| {
                        let top = ui.cursor().min;
                        place_row(
                            ui,
                            &mut act,
                            &mut editing,
                            &mut thumbs,
                            1,
                            0,
                            "Test place",
                            [1.0, 0.2, 0.2],
                            (1.0, 2.0, 10.0),
                            None,
                            true,
                        );
                        // The name button fills the row after the pin dot — aim
                        // well inside it.
                        name = egui::pos2(top.x + 70.0, (top.y + ui.cursor().min.y) * 0.5);
                    });
            });
            if act.route.is_some() {
                out.route = act.route;
            }
            if act.teleport.is_some() {
                out.teleport = act.teleport;
            }
            if act.update.is_some() {
                out.update = act.update;
            }
            if act.delete_place.is_some() {
                out.delete_place = act.delete_place;
            }
        }
        out
    }

    /// Sweep the menu for the entry that routes to the place.
    fn sweep_for_route(long_touch: bool) -> Actions {
        for step in 0..40 {
            let act = click_place_menu(long_touch, 6.0 + step as f32 * 4.0);
            if act.route.is_some() {
                return act;
            }
        }
        Actions::default()
    }

    /// Routing to a place has to survive its move off the row and into the menu.
    ///
    /// A saved place is the destination that needs no typing, so this is the
    /// action that matters most on a phone — and it now lives behind a popup
    /// opened from a widget that also senses drags, which is the arrangement that
    /// has eaten a press in this row before (see the module docs).
    #[test]
    fn the_context_menu_routes_to_the_place() {
        let act = sweep_for_route(false);
        assert_eq!(
            act.route,
            Some(("Test place".to_string(), 1.0, 2.0)),
            "no sweep of the context menu ever reached Route here"
        );
        assert!(
            act.teleport.is_none(),
            "routing to a place must not also fly to it"
        );
    }

    /// The same menu, reached the way a phone reaches it — and without the row
    /// acting on the press that opened it.
    ///
    /// egui synthesises a `clicked()` alongside a long press, so that a widget
    /// with no menu still responds to the hold (`interaction.rs`). Without
    /// `place_row`'s `!long_touched()` guard, a long press would open the menu and
    /// fly the camera to the place behind it at the same time — and the menu
    /// would then be covering somewhere else entirely.
    #[test]
    fn a_long_touch_opens_the_menu_without_jumping_to_the_place() {
        let act = sweep_for_route(true);
        assert_eq!(
            act.route,
            Some(("Test place".to_string(), 1.0, 2.0)),
            "a long touch never opened the context menu"
        );
        assert!(
            act.teleport.is_none(),
            "a long press opened the menu and flew to the place as well"
        );
    }

    /// A press-and-release on a row must act on the row, and must not leave a
    /// drag hanging off it.
    ///
    /// This is the guard against wrapping a row in `Ui::dnd_drag_source` again
    /// (see the module docs): that lays a drag-*only* widget over the whole row,
    /// which egui marks as dragged on mouse-down with no movement threshold, so
    /// every press in the row — the name, ✏, 🗑 — became a drag and no click
    /// ever landed.
    #[test]
    fn clicking_a_row_acts_on_it_and_starts_no_drag() {
        let (act, dragged) = drive(false);
        assert!(
            act.teleport.is_some(),
            "clicking the name must jump to the place"
        );
        assert!(!dragged, "a plain click must not start a drag");
    }

    /// The other direction: press and move *does* pick the row up, or the tree
    /// can't be rearranged at all.
    #[test]
    fn dragging_a_row_by_its_name_picks_it_up() {
        let (act, dragged) = drive(true);
        assert!(dragged, "press-and-move must start a drag");
        assert!(
            act.teleport.is_none(),
            "a drag must not also fire the row's click"
        );
    }

    /// An open rename field must not push the panel wider — and especially not a
    /// little wider every frame.
    ///
    /// A `Panel` stores its size from the rect its contents came out at, so any
    /// row that overflows the panel widens it, which widens the row's available
    /// width, which overflows again: the panel walks across the screen for as
    /// long as the field is open. Renders with the field focused (the state that
    /// triggered it) and pins the width over several frames.
    #[test]
    fn an_open_rename_field_does_not_widen_the_panel() {
        let ctx = egui::Context::default();
        // The shipped theme, not `Visuals::dark()`: these tests are about
        // widths and hit-testing, both of which are padding-dependent.
        theme::install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));

        let mut editing = Some(Rename {
            item: Item::Folder(1),
            buf: "New folder".to_string(),
            focus: true,
        });
        let mut widths = Vec::new();

        for _ in 0..8 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            let mut act = Actions::default();
            let _ = ctx.run_ui(raw, |ui| {
                let panel = crate::systems::egui_manager::responsive_right_panel(
                    "bookmarks_panel",
                    false,
                    screen.width(),
                    300.0,
                )
                .show(ui, |ui| {
                    ui.heading("Places");
                    // Long enough to overflow if the field ever asks for more
                    // than the row has.
                    folder_row(
                        ui,
                        &mut act,
                        &mut editing,
                        1,
                        0,
                        "A folder name long enough to need truncating",
                        [0.2, 0.5, 0.9],
                        false,
                        0,
                    );
                });
                widths.push(panel.response.rect.width());
            });
        }

        let last = *widths.last().unwrap();
        let prev = widths[widths.len() - 2];
        assert!(
            (last - prev).abs() < 0.5,
            "panel still growing with the rename field open: {widths:?}"
        );
        assert!(
            last <= 320.0,
            "panel grew past its 300 pt width: {widths:?}"
        );
    }

    /// The whole gesture: drag a place onto a folder row and drop it there.
    ///
    /// The folder's drop target is the `Ui::horizontal` response covering the
    /// row, which only works because that response is a registered widget and so
    /// reports `contains_pointer` — a silently dead drop target would leave the
    /// tree looking right and refusing to be rearranged.
    #[test]
    fn dropping_a_place_on_a_folder_row_moves_it_there() {
        let ctx = egui::Context::default();
        // The shipped theme, not `Visuals::dark()`: these tests are about
        // widths and hit-testing, both of which are padding-dependent.
        theme::install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));

        const FOLDER: u64 = 7;
        const PLACE: u64 = 1;
        let mut folder_at = egui::Pos2::ZERO;
        let mut place_at = egui::Pos2::ZERO;
        let mut dropped = None;

        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        for frame in 0..6 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            match frame {
                1 => raw.events.push(egui::Event::PointerMoved(place_at)),
                2 => raw.events.push(button(place_at, true)),
                // Onto the folder — far enough from the press origin that egui
                // stops treating the gesture as a click. Twice, so the row has a
                // frame with the pointer over it before the release.
                3 | 4 => raw.events.push(egui::Event::PointerMoved(folder_at)),
                5 => raw.events.push(button(folder_at, false)),
                _ => {}
            }

            let mut act = Actions::default();
            let mut editing = None;
            let mut thumbs = HashMap::new();
            let _ = ctx.run_ui(raw, |ui| {
                egui::Panel::right("places_test")
                    .default_size(300.0)
                    .show(ui, |ui| {
                        let top = ui.cursor().min;
                        folder_row(
                            ui,
                            &mut act,
                            &mut editing,
                            FOLDER,
                            0,
                            "Folder",
                            [0.2, 0.5, 0.9],
                            false,
                            0,
                        );
                        let mid = ui.cursor().min;
                        // Left of the ✏/🗑 buttons, so the drop lands on the row
                        // rather than on a button.
                        folder_at = egui::pos2(top.x + 80.0, (top.y + mid.y) * 0.5);
                        place_row(
                            ui,
                            &mut act,
                            &mut editing,
                            &mut thumbs,
                            PLACE,
                            0,
                            "Test place",
                            [1.0, 0.2, 0.2],
                            (1.0, 2.0, 10.0),
                            None,
                            true,
                        );
                        place_at = egui::pos2(mid.x + 70.0, (mid.y + ui.cursor().min.y) * 0.5);
                    });
            });
            if let Some(drop) = act.drop {
                dropped = Some(drop);
            }
        }

        match dropped {
            Some((Item::Place(id), Some(folder))) => {
                assert_eq!(id, PLACE);
                assert_eq!(folder, FOLDER);
            }
            other => panic!(
                "dragging a place onto a folder row must drop it into that folder, got {}",
                match other {
                    None => "no drop at all".to_string(),
                    Some((Item::Folder(id), t)) => format!("folder {id} -> {t:?}"),
                    Some((Item::Place(id), t)) => format!("place {id} -> {t:?}"),
                }
            ),
        }
    }
}
