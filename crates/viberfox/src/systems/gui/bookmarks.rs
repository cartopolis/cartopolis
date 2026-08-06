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

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::GridCell;

use crate::platform::http;
use crate::resources::CameraState;
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE};
use crate::systems::geo_nav::{
    nominatim_base_url, world_to_lat_lng, CameraPose, GeoNavState, Teleport,
};
use crate::systems::map_stream::MapStream;
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
    rename: Option<(Item, String)>,
    recolor: Option<(u64, [f32; 3])>,
    toggle_collapse: Option<u64>,
    /// (what was dragged, which folder it was dropped on — `None` = top level).
    drop: Option<(Item, Option<u64>)>,
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
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::click());
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
        ui.painter()
            .add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
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
        commit = ui.button("✔").on_hover_text("Apply").clicked();
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
/// Click a name to jump there (camera view is preserved), 🔄 to update it to the
/// current view, ✏ to rename, 🗑 to delete; drag a row onto a folder to move it,
/// and use the folder's colour swatch to recolour its pins on the map.
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

    // Current location + view, for the per-place "update to here" action.
    let current = camera.single().ok().zip(stream.geo_anchor()).map(
        |((cell, tf), anchor)| {
            let wx = cell.x as f64 * WORLD_CELL_EDGE as f64 + tf.translation.x as f64;
            let wz = cell.z as f64 * WORLD_CELL_EDGE as f64 + tf.translation.z as f64;
            let (lat, lng) = world_to_lat_lng(anchor, wx, wz);
            let alt = cell.y as f64 * WORLD_CELL_EDGE as f64 + tf.translation.y as f64;
            (lat, lng, alt, CameraPose::capture(&camera_state))
        },
    );

    let mut rows = Vec::new();
    flatten(&store, None, 0, &mut rows);

    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let ui_root = egui.ui_mut();
    let mut act = Actions::default();
    let mut editing = bookmarks.editing.take();

    crate::systems::egui_manager::responsive_right_panel(
        "bookmarks_panel",
        compact,
        viewport_w,
        300.0,
    )
    .show_collapsible(ui_root, &mut open, |ui| {
        let panel = crate::resources::RightPanel::Places;
        act.close = crate::systems::theme::panel_header_with(
            ui,
            panel.icons(),
            panel.label(),
            |ui| {
                // Spelled out rather than an icon: 📁 is not in the bundled icon
                // font and renders as a tofu box.
                if ui
                    .button("+ Folder")
                    .on_hover_text("New folder — drag places into it to group them")
                    .clicked()
                {
                    act.new_folder = true;
                }
            },
        );
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
                    Row::Folder { id, depth, name, color, collapsed, count } => {
                        folder_row(
                            ui, &mut act, &mut editing, *id, *depth, name, *color, *collapsed,
                            *count,
                        );
                    }
                    Row::Place { id, depth, name, color, lat, lng, altitude } => {
                        place_row(
                            ui,
                            &mut act,
                            &mut editing,
                            *id,
                            *depth,
                            name,
                            *color,
                            (*lat, *lng, *altitude),
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
                let stroke = egui::Stroke::new(
                    1.0,
                    if hot {
                        theme::FOX
                    } else {
                        theme::LINE
                    },
                );
                ui.painter().rect_stroke(
                    rect.shrink(1.0),
                    4.0,
                    stroke,
                    egui::StrokeKind::Inside,
                );
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
    drag_ghost(&egui.ctx, &rows);

    let close = act.close;
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
                if ui
                    .small_button("🗑")
                    .on_hover_text("Delete the folder (its places move up a level)")
                    .clicked()
                {
                    act.delete_folder = Some(id);
                }
                if ui.small_button("✏").on_hover_text("Rename").clicked() {
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

/// One place row: a coloured pin dot matching its map pin, the name (click to
/// jump), and the update/rename/delete actions.
#[allow(clippy::too_many_arguments)]
fn place_row(
    ui: &mut egui::Ui,
    act: &mut Actions,
    editing: &mut Option<Rename>,
    id: u64,
    depth: usize,
    name: &str,
    color: [f32; 3],
    (lat, lng, altitude): (f64, f64, f64),
    can_update: bool,
) {
    // Not a drag source while renaming — see `folder_row`.
    if rename_row(ui, act, editing, Item::Place(id), depth) {
        return;
    }

    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * INDENT);
        // The dot is the map pin's colour, so the panel and the map read as
        // one thing.
        ui.label(egui::RichText::new("📍").size(12.0).color(egui_color(color)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("🗑").on_hover_text("Delete").clicked() {
                act.delete_place = Some(id);
            }
            if ui.small_button("✏").on_hover_text("Rename").clicked() {
                *editing = Some(Rename {
                    item: Item::Place(id),
                    buf: name.to_string(),
                    focus: true,
                });
            }
            if ui
                .add_enabled(can_update, egui::Button::new("🔄").small())
                .on_hover_text("Update to current position and view")
                .clicked()
            {
                act.update = Some(id);
            }
            fill_remaining(ui, |ui| {
                // The name button is also the drag handle (see `drag_handle`):
                // sensing both, egui waits to see whether the press turns into a
                // drag, so a click still jumps.
                let name = ui
                    .add(
                        egui::Button::new(egui::RichText::new(name).size(13.0))
                            .wrap_mode(egui::TextWrapMode::Truncate)
                            .min_size(egui::vec2(ui.available_width(), 0.0))
                            .sense(egui::Sense::click_and_drag()),
                    )
                    .on_hover_text(format!(
                        "{lat:.5}, {lng:.5}, {altitude:.0} m — click to jump, drag into a folder"
                    ));
                name.dnd_set_drag_payload(Item::Place(id));
                if name.clicked() {
                    // Keep the *current* camera mode and move it to the place's
                    // spot, rather than snapping back to the mode it was saved in
                    // (`pose: None` skips `CameraPose::apply`, so `apply_teleport`
                    // repositions the mode you're in — in Follow that means the
                    // avatar walks there).
                    act.teleport = Some(Teleport {
                        lat,
                        lng,
                        altitude: Some(altitude),
                        pose: None,
                    });
                }
            });
        });
    });
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
        (Row::Folder { id, name, color, .. }, Item::Folder(dragged)) if *id == dragged => {
            Some((name.as_str(), *color))
        }
        (Row::Place { id, name, color, .. }, Item::Place(dragged)) if *id == dragged => {
            Some((name.as_str(), *color))
        }
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
        store.data.bookmarks.retain(|b| b.id != id);
        store.dirty = true;
    }
    if let Some(id) = act.delete_folder {
        store.remove_folder(id);
    }
    if let Some(tp) = act.teleport {
        nav.pending_teleport = Some(tp);
    }
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
            let _ = ctx.run_ui(raw, |ui| {
                egui::Panel::right("places_test")
                    .default_size(300.0)
                    .show(ui, |ui| {
                        let top = ui.cursor().min;
                        place_row(
                            ui,
                            &mut act,
                            &mut editing,
                            1,
                            0,
                            "Test place",
                            [1.0, 0.2, 0.2],
                            (1.0, 2.0, 10.0),
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
            let _ = ctx.run_ui(raw, |ui| {
                egui::Panel::right("places_test")
                    .default_size(300.0)
                    .show(ui, |ui| {
                        let top = ui.cursor().min;
                        folder_row(
                            ui, &mut act, &mut editing, FOLDER, 0, "Folder", [0.2, 0.5, 0.9],
                            false, 0,
                        );
                        let mid = ui.cursor().min;
                        // Left of the ✏/🗑 buttons, so the drop lands on the row
                        // rather than on a button.
                        folder_at = egui::pos2(top.x + 80.0, (top.y + mid.y) * 0.5);
                        place_row(
                            ui,
                            &mut act,
                            &mut editing,
                            PLACE,
                            0,
                            "Test place",
                            [1.0, 0.2, 0.2],
                            (1.0, 2.0, 10.0),
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
