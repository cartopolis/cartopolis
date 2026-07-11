//! Prim UI system (Phase 5 - ADR-017).
//!
//! Renders context menus and edit dialogs using egui.

use super::egui_manager::EguiManager;
use crate::components::{NeedsMeshRebuild, Prim, PrimShape, Region, Selected};
use crate::resources::{
    ContextMenuState, Database, EditDialogState, GameState, PrimTextureCache, TextureLibrary,
};
use crate::resources::OnlineSession;
use bevy::pbr::StandardMaterial;
use bevy::prelude::*;
use egui::Window;
use crate::resources::AiAssistantState;
use vibe_core::NetMessage;

/// Top menu bar: `File ▸ Exit`, `View ▸ AI Panel / Dev Panel`, and `Layers`
/// (a top-level toggle for the Layers side panel).
pub fn render_menu_bar(
    mut egui: ResMut<EguiManager>,
    mut ai_state: ResMut<AiAssistantState>,
    mut dev_state: ResMut<crate::resources::DevPanelState>,
    mut layers: ResMut<crate::systems::data_layers::DataLayers>,
    mut app_exit: EventWriter<AppExit>,
) {
    let ctx = egui.ctx_mut();
    egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Exit").clicked() {
                    app_exit.write(AppExit::Success);
                    ui.close_menu();
                }
            });
            ui.menu_button("View", |ui| {
                let ai_label = if ai_state.open {
                    "Hide AI Panel"
                } else {
                    "Show AI Panel"
                };
                if ui.button(ai_label).clicked() {
                    ai_state.open = !ai_state.open;
                    ui.close_menu();
                }
                let dev_label = if dev_state.open {
                    "Hide Dev Panel"
                } else {
                    "Show Dev Panel"
                };
                if ui.button(dev_label).clicked() {
                    dev_state.open = !dev_state.open;
                    ui.close_menu();
                }
            });
            // Top-level toggle, like a browser's sidebar button.
            if ui.button("Layers").clicked() {
                layers.panel_open = !layers.panel_open;
            }
        });
    });
}

/// Toggle the AI panel with Ctrl+Shift+A.
pub fn toggle_ai_panel_shortcut(
    keyboard: Res<ButtonInput<KeyCode>>,
    mut ai_state: ResMut<AiAssistantState>,
) {
    let ctrl =
        keyboard.pressed(KeyCode::ControlLeft) || keyboard.pressed(KeyCode::ControlRight);
    let shift =
        keyboard.pressed(KeyCode::ShiftLeft) || keyboard.pressed(KeyCode::ShiftRight);
    if ctrl && shift && keyboard.just_pressed(KeyCode::KeyA) {
        ai_state.open = !ai_state.open;
    }
}

/// Toggle the developer panel with Ctrl+Shift+D.
pub fn toggle_dev_panel_shortcut(
    keyboard: Res<ButtonInput<KeyCode>>,
    mut dev_state: ResMut<crate::resources::DevPanelState>,
) {
    let ctrl =
        keyboard.pressed(KeyCode::ControlLeft) || keyboard.pressed(KeyCode::ControlRight);
    let shift =
        keyboard.pressed(KeyCode::ShiftLeft) || keyboard.pressed(KeyCode::ShiftRight);
    if ctrl && shift && keyboard.just_pressed(KeyCode::KeyD) {
        dev_state.open = !dev_state.open;
    }
}

/// Developer panel (View ▸ Dev Panel or Ctrl+Shift+D): a day/night slider that drives
/// the sun position so the lighting can be tested at any time of day.
pub fn render_dev_panel(
    mut egui: ResMut<EguiManager>,
    mut dev: ResMut<crate::resources::DevPanelState>,
) {
    if !dev.open {
        return;
    }
    let ctx = egui.ctx_mut();
    let mut open = true;
    egui::Window::new("Dev — Lighting")
        .open(&mut open)
        .default_width(260.0)
        .show(ctx, |ui| {
            ui.label("Day / night cycle");
            ui.add(
                egui::Slider::new(&mut dev.time_of_day, 0.0..=24.0)
                    .suffix(" h")
                    .step_by(0.1),
            );
            let h = dev.time_of_day.floor() as i32 % 24;
            let m = (dev.time_of_day.fract() * 60.0).floor() as i32;
            ui.label(format!("Time: {:02}:{:02}", h, m));
            ui.horizontal(|ui| {
                if ui.button("Sunrise").clicked() {
                    dev.time_of_day = 6.0;
                }
                if ui.button("Noon").clicked() {
                    dev.time_of_day = 12.0;
                }
                if ui.button("Sunset").clicked() {
                    dev.time_of_day = 18.0;
                }
                if ui.button("Midnight").clicked() {
                    dev.time_of_day = 0.0;
                }
            });
            ui.separator();
            ui.label("Distance fog");
            ui.add(
                egui::Slider::new(&mut dev.fog_density, 0.0..=1.0)
                    .text("density")
                    .step_by(0.01),
            );
            ui.separator();
            ui.label("Moon");
            ui.add(
                egui::Slider::new(
                    &mut dev.moon_age_days,
                    0.0..=crate::utils::sun::LUNAR_CYCLE_DAYS,
                )
                .text("age (days)")
                .step_by(0.1),
            );
            let lit = crate::utils::sun::moon_illuminated_fraction(dev.moon_age_days);
            ui.label(format!("{:.0}% lit (0 = new, ~14.8 = full)", lit * 100.0));
            ui.separator();
            ui.horizontal(|ui| {
                ui.checkbox(&mut dev.auto_cycle, "Quick cycle");
                ui.add(
                    egui::DragValue::new(&mut dev.cycle_seconds)
                        .suffix(" s")
                        .range(1.0..=120.0)
                        .speed(0.5),
                );
            });
        });
    // Closing via the window's [x] only writes when it actually changed, to avoid
    // marking the resource changed every frame (which would re-run the lighting update).
    if !open && dev.open {
        dev.open = false;
    }
}

/// Fill the edit dialog's fields and revert snapshots from a prim. Leaves `visible` and
/// `active_tab` untouched, so it serves both opening the dialog and following a selection
/// change. Sets `is_new = false` and `prim_id` to this prim.
pub fn populate_edit_dialog(
    edit_dialog: &mut EditDialogState,
    prim: &Prim,
    transform: &Transform,
) {
    edit_dialog.prim_id = Some(prim.id);
    edit_dialog.is_new = false;
    edit_dialog.name = prim.name.clone();
    let color = {
        let c = prim.color.to_linear();
        [c.red, c.green, c.blue]
    };
    edit_dialog.color = color;
    let shape = format!("{:?}", prim.shape).to_lowercase();
    edit_dialog.shape = shape.clone();
    let pos = transform.translation;
    edit_dialog.position = [pos.x, pos.y, pos.z];
    let rot = transform.rotation.to_euler(EulerRot::XYZ);
    edit_dialog.rotation = [rot.0, rot.1, rot.2];
    let scale_v = transform.scale;
    edit_dialog.scale = [scale_v.x, scale_v.y, scale_v.z];
    edit_dialog.texture_id = prim.texture_id.clone();
    edit_dialog.path_cut_begin = prim.path_cut_begin;
    edit_dialog.path_cut_end = prim.path_cut_end;
    edit_dialog.hollow = prim.hollow;
    edit_dialog.surface = prim.surface;
    edit_dialog.repeats_per_meter = 0.0;
    edit_dialog.warp = crate::resources::PrimWarp {
        twist_begin: prim.twist_begin,
        twist_end: prim.twist_end,
        taper_x: prim.taper_x,
        taper_y: prim.taper_y,
        top_shear_x: prim.top_shear_x,
        top_shear_y: prim.top_shear_y,
        slice_begin: prim.slice_begin,
        slice_end: prim.slice_end,
    };
    // Snapshot for Cancel revert.
    edit_dialog.original_name = prim.name.clone();
    edit_dialog.original_color = color;
    edit_dialog.original_shape = shape;
    edit_dialog.original_position = edit_dialog.position;
    edit_dialog.original_rotation = edit_dialog.rotation;
    edit_dialog.original_scale = edit_dialog.scale;
    edit_dialog.original_texture_id = prim.texture_id.clone();
    edit_dialog.original_path_cut_begin = prim.path_cut_begin;
    edit_dialog.original_path_cut_end = prim.path_cut_end;
    edit_dialog.original_hollow = prim.hollow;
    edit_dialog.original_warp = edit_dialog.warp;
    edit_dialog.original_surface = prim.surface;
    edit_dialog.texture_picker_open = false;
}

/// While the edit dialog is open, keep it pointed at the currently selected prim so
/// clicking a different prim updates the dialog instead of leaving it on the old one.
pub fn sync_dialog_to_selection(
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    prim_query: Query<(&Prim, &Transform)>,
) {
    // Only follow selection for an open editor of an existing prim (never clobber a
    // half-filled "Create Prim" dialog).
    if !edit_dialog.visible || edit_dialog.is_new {
        return;
    }
    let Some(selected) = game_state.selected_prim_id else {
        return;
    };
    if edit_dialog.prim_id == Some(selected) {
        return; // already showing the selected prim
    }
    for (prim, transform) in prim_query.iter() {
        if prim.id == selected {
            populate_edit_dialog(&mut edit_dialog, prim, transform);
            game_state.editing_prim_id = Some(selected);
            break;
        }
    }
}

/// Render context menu when a prim is right-clicked
pub fn render_context_menu(
    mut commands: Commands,
    mut egui: ResMut<EguiManager>,
    mut context_menu: ResMut<ContextMenuState>,
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    prim_query: Query<(Entity, &Prim, &Transform)>,
    selected_query: Query<Entity, With<Selected>>,
) {
    if !context_menu.visible {
        return;
    }

    let ctx = egui.ctx_mut();
    let mut menu_open = true;

    // Position the context menu at the mouse click location
    let menu_pos = egui::pos2(context_menu.screen_pos.x, context_menu.screen_pos.y);

    Window::new("Context Menu")
        .open(&mut menu_open)
        .fixed_pos(menu_pos)
        .show(ctx, |ui| {
            if let Some(prim_id) = context_menu.hit_prim_id {
                // Clicked on an existing prim
                if ui.button("Edit Prim (E)").clicked() {
                    for entity in selected_query.iter() {
                        commands.entity(entity).remove::<Selected>();
                    }
                    for (entity, prim, transform) in prim_query.iter() {
                        if prim.id == prim_id {
                            commands.entity(entity).insert(Selected);
                            game_state.selected_prim_id = Some(prim_id);
                            game_state.editing_prim_id = Some(prim_id);
                            populate_edit_dialog(&mut edit_dialog, prim, transform);
                            edit_dialog.visible = true;
                            break;
                        }
                    }
                    context_menu.visible = false;
                }

                if ui.button("Delete Prim (D)").clicked() {
                    tracing::info!(prim_id, "deleting prim");
                    game_state.prims_to_delete.push(prim_id);
                    context_menu.visible = false;
                }
            } else {
                // Clicked on empty space - create new prim
                if ui.button("Create Prim (C)").clicked() {
                    edit_dialog.prim_id = None;
                    edit_dialog.is_new = true;
                    edit_dialog.name = "Prim".to_string();
                    edit_dialog.color = [0.5, 0.5, 0.5]; // neutral gray
                    edit_dialog.shape = "box".to_string();
                    edit_dialog.texture_id = None;
                    edit_dialog.texture_picker_open = false;
                    edit_dialog.path_cut_begin = 0.0;
                    edit_dialog.path_cut_end = 1.0;
                    edit_dialog.hollow = 0.0;
                    edit_dialog.warp = crate::resources::PrimWarp::default();
                    edit_dialog.surface = vibe_core::PrimSurface::default();
                    edit_dialog.repeats_per_meter = 0.0;
                    let hit = context_menu.hit_point;
                    // Offset Y so the prim rests on the surface instead of being centred in it.
                    edit_dialog.position = [hit.x, hit.y + 0.5, hit.z];
                    edit_dialog.rotation = [0.0, 0.0, 0.0];
                    edit_dialog.scale = [1.0, 1.0, 1.0];
                    edit_dialog.visible = true;
                    game_state.editing_prim_id = None;
                    context_menu.visible = false;
                }
            }

            if ui.button("Cancel (ESC)").clicked() {
                context_menu.visible = false;
            }
        });

    if !menu_open {
        context_menu.visible = false;
    }
}

/// Render the prim edit dialog
pub fn render_edit_dialog(
    mut egui: ResMut<EguiManager>,
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    texture_lib: Res<TextureLibrary>,
    mut gizmo_state: ResMut<crate::systems::gizmo::GizmoState>,
) {
    if !edit_dialog.visible {
        return;
    }

    let ctx = egui.ctx_mut();
    let mut dialog_open = true;

    let title = if edit_dialog.is_new {
        "Create Prim"
    } else {
        "Edit Prim"
    };

    Window::new(title)
        .open(&mut dialog_open)
        .default_width(320.0)
        .show(ctx, |ui| {
            // Tab bar
            ui.horizontal(|ui| {
                ui.selectable_value(&mut edit_dialog.active_tab, 0, "Edit");
                ui.selectable_value(&mut edit_dialog.active_tab, 1, "Object");
                ui.selectable_value(&mut edit_dialog.active_tab, 2, "Texture");
            });
            ui.separator();

            match edit_dialog.active_tab {
                // ── Edit tab ────────────────────────────────────────────────
                0 => {
                    use crate::systems::gizmo::GizmoMode;
                    egui::Grid::new("edit_grid")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Name");
                            ui.text_edit_singleline(&mut edit_dialog.name);
                            ui.end_row();

                            ui.label("Tool");
                            ui.horizontal(|ui| {
                                ui.radio_value(
                                    &mut gizmo_state.mode,
                                    GizmoMode::Translate,
                                    "Move (T)",
                                );
                                ui.radio_value(
                                    &mut gizmo_state.mode,
                                    GizmoMode::Rotate,
                                    "Rotate (R)",
                                );
                                ui.radio_value(
                                    &mut gizmo_state.mode,
                                    GizmoMode::Scale,
                                    "Scale (S)",
                                );
                            });
                            ui.end_row();
                        });
                }
                // ── Object tab ──────────────────────────────────────────────
                1 => {
                    egui::Grid::new("object_grid")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Building Block Type");
                            egui::ComboBox::from_id_salt("shape_combo")
                                .selected_text(shape_display_name(&edit_dialog.shape))
                                .show_ui(ui, |ui| {
                                    for (key, label) in SHAPES {
                                        ui.selectable_value(
                                            &mut edit_dialog.shape,
                                            key.to_string(),
                                            *label,
                                        );
                                    }
                                });
                            ui.end_row();
                        });

                    ui.separator();

                    // Position / Size / Rotation
                    egui::Grid::new("transform_grid")
                        .num_columns(4)
                        .spacing([4.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Position (meters)");
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.position[0])
                                    .prefix("X ")
                                    .speed(0.1),
                            );
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.position[1])
                                    .prefix("Y ")
                                    .speed(0.1),
                            );
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.position[2])
                                    .prefix("Z ")
                                    .speed(0.1),
                            );
                            ui.end_row();

                            ui.label("Size (meters)");
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.scale[0])
                                    .prefix("X ")
                                    .speed(0.1)
                                    .range(0.001..=f32::MAX),
                            );
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.scale[1])
                                    .prefix("Y ")
                                    .speed(0.1)
                                    .range(0.001..=f32::MAX),
                            );
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.scale[2])
                                    .prefix("Z ")
                                    .speed(0.1)
                                    .range(0.001..=f32::MAX),
                            );
                            ui.end_row();

                            ui.label("Rotation (degrees)");
                            let mut rot_x = edit_dialog.rotation[0].to_degrees();
                            if ui
                                .add(
                                    egui::DragValue::new(&mut rot_x)
                                        .prefix("X ")
                                        .speed(1.0)
                                        .suffix("°"),
                                )
                                .changed()
                            {
                                edit_dialog.rotation[0] = rot_x.to_radians();
                            }
                            let mut rot_y = edit_dialog.rotation[1].to_degrees();
                            if ui
                                .add(
                                    egui::DragValue::new(&mut rot_y)
                                        .prefix("Y ")
                                        .speed(1.0)
                                        .suffix("°"),
                                )
                                .changed()
                            {
                                edit_dialog.rotation[1] = rot_y.to_radians();
                            }
                            let mut rot_z = edit_dialog.rotation[2].to_degrees();
                            if ui
                                .add(
                                    egui::DragValue::new(&mut rot_z)
                                        .prefix("Z ")
                                        .speed(1.0)
                                        .suffix("°"),
                                )
                                .changed()
                            {
                                edit_dialog.rotation[2] = rot_z.to_radians();
                            }
                            ui.end_row();
                        });

                    ui.separator();

                    // Path Cut Begin and End
                    ui.label("Path Cut Begin and End");
                    ui.horizontal(|ui| {
                        let pce = edit_dialog.path_cut_end;
                        let pcb = edit_dialog.path_cut_begin;
                        ui.label("B");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.path_cut_begin)
                                .speed(0.005)
                                .range(0.0..=pce)
                                .min_decimals(3)
                                .max_decimals(3),
                        );
                        ui.label("E");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.path_cut_end)
                                .speed(0.005)
                                .range(pcb..=1.0)
                                .min_decimals(3)
                                .max_decimals(3),
                        );
                    });

                    ui.add_space(4.0);

                    // Hollow (stored 0–0.95, displayed 0–95)
                    ui.label("Hollow");
                    let mut hollow_pct = edit_dialog.hollow * 100.0;
                    if ui
                        .add(
                            egui::DragValue::new(&mut hollow_pct)
                                .speed(0.5)
                                .range(0.0..=95.0)
                                .min_decimals(1)
                                .max_decimals(1),
                        )
                        .changed()
                    {
                        edit_dialog.hollow = hollow_pct / 100.0;
                    }

                    ui.add_space(4.0);

                    // Twist Begin and End (degrees, −360..360)
                    ui.label("Twist Begin and End");
                    ui.horizontal(|ui| {
                        ui.label("B");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.twist_begin)
                                .speed(1.0)
                                .range(-360.0..=360.0)
                                .suffix("°"),
                        );
                        ui.label("E");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.twist_end)
                                .speed(1.0)
                                .range(-360.0..=360.0)
                                .suffix("°"),
                        );
                    });

                    ui.add_space(4.0);

                    // Taper (−1..1 per axis)
                    ui.label("Taper");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.taper_x)
                                .prefix("X ")
                                .speed(0.01)
                                .range(-1.0..=1.0)
                                .max_decimals(2),
                        );
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.taper_y)
                                .prefix("Y ")
                                .speed(0.01)
                                .range(-1.0..=1.0)
                                .max_decimals(2),
                        );
                    });

                    ui.add_space(4.0);

                    // Top Shear (−0.5..0.5 per axis)
                    ui.label("Top Shear");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.top_shear_x)
                                .prefix("X ")
                                .speed(0.01)
                                .range(-0.5..=0.5)
                                .max_decimals(2),
                        );
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.top_shear_y)
                                .prefix("Y ")
                                .speed(0.01)
                                .range(-0.5..=0.5)
                                .max_decimals(2),
                        );
                    });

                    ui.add_space(4.0);

                    // Slice Begin and End (trims the path 0..1, begin ≤ end)
                    ui.label("Slice Begin and End");
                    ui.horizontal(|ui| {
                        let sb = edit_dialog.warp.slice_begin;
                        let se = edit_dialog.warp.slice_end;
                        ui.label("B");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.slice_begin)
                                .speed(0.005)
                                .range(0.0..=se)
                                .min_decimals(3)
                                .max_decimals(3),
                        );
                        ui.label("E");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.slice_end)
                                .speed(0.005)
                                .range(sb..=1.0)
                                .min_decimals(3)
                                .max_decimals(3),
                        );
                    });
                }
                // ── Texture tab ─────────────────────────────────────────────
                _ => {
                    egui::Grid::new("texture_grid")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Color");
                            let mut srgb = [
                                (edit_dialog.color[0] * 255.0) as u8,
                                (edit_dialog.color[1] * 255.0) as u8,
                                (edit_dialog.color[2] * 255.0) as u8,
                            ];
                            if ui.color_edit_button_srgb(&mut srgb).changed() {
                                edit_dialog.color = [
                                    srgb[0] as f32 / 255.0,
                                    srgb[1] as f32 / 255.0,
                                    srgb[2] as f32 / 255.0,
                                ];
                            }
                            ui.end_row();

                            ui.label("Texture");
                            ui.horizontal(|ui| {
                                ui.label(
                                    edit_dialog.texture_id.as_deref().unwrap_or("None"),
                                );
                                if !texture_lib.entries.is_empty()
                                    && ui.button("Pick…").clicked()
                                {
                                    edit_dialog.texture_picker_open =
                                        !edit_dialog.texture_picker_open;
                                }
                                if edit_dialog.texture_id.is_some()
                                    && ui.button("Clear").clicked()
                                {
                                    edit_dialog.texture_id = None;
                                }
                            });
                            ui.end_row();

                            // Transparency (SL "Transparency %": 0 = opaque, higher = more see-through).
                            ui.label("Transparency");
                            let mut transparency = (1.0 - edit_dialog.surface.alpha) * 100.0;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut transparency)
                                        .speed(0.5)
                                        .range(0.0..=100.0)
                                        .suffix(" %")
                                        .max_decimals(1),
                                )
                                .changed()
                            {
                                edit_dialog.surface.alpha =
                                    (1.0 - transparency / 100.0).clamp(0.0, 1.0);
                            }
                            ui.end_row();

                            // Glow (0–1; emissive surface glow).
                            ui.label("Glow");
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.surface.glow)
                                    .speed(0.01)
                                    .range(0.0..=1.0)
                                    .max_decimals(2),
                            );
                            ui.end_row();

                            ui.label("Full Bright");
                            ui.checkbox(&mut edit_dialog.surface.full_bright, "");
                            ui.end_row();
                        });

                    ui.separator();

                    // Repeats Per Face (tiling count per axis, with optional flip).
                    ui.label("Repeats Per Face");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.surface.repeat_u)
                                .prefix("U ")
                                .speed(0.05)
                                .range(0.0..=100.0)
                                .max_decimals(3),
                        );
                        ui.checkbox(&mut edit_dialog.surface.flip_u, "Flip");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.surface.repeat_v)
                                .prefix("V ")
                                .speed(0.05)
                                .range(0.0..=100.0)
                                .max_decimals(3),
                        );
                        ui.checkbox(&mut edit_dialog.surface.flip_v, "Flip");
                    });

                    ui.add_space(4.0);

                    // Texture rotation (degrees, −360..360).
                    ui.label("Rotation (degrees)");
                    ui.add(
                        egui::DragValue::new(&mut edit_dialog.surface.rotation)
                            .speed(1.0)
                            .range(-360.0..=360.0)
                            .suffix("°"),
                    );

                    ui.add_space(4.0);

                    // Repeats Per Meter: derives per-face repeats from object size on Apply.
                    ui.label("Repeats Per Meter");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.repeats_per_meter)
                                .speed(0.05)
                                .range(0.0..=10.0)
                                .max_decimals(3),
                        );
                        if ui.button("Apply").clicked() && edit_dialog.repeats_per_meter > 0.0 {
                            let rpm = edit_dialog.repeats_per_meter;
                            edit_dialog.surface.repeat_u = rpm * edit_dialog.scale[0];
                            edit_dialog.surface.repeat_v = rpm * edit_dialog.scale[1];
                        }
                    });

                    ui.add_space(4.0);

                    // Texture offset (fraction of a repeat).
                    ui.label("Offset");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.surface.offset_u)
                                .prefix("U ")
                                .speed(0.005)
                                .range(-1.0..=1.0)
                                .max_decimals(3),
                        );
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.surface.offset_v)
                                .prefix("V ")
                                .speed(0.005)
                                .range(-1.0..=1.0)
                                .max_decimals(3),
                        );
                    });
                }
            }

            ui.separator();

            // Dialog buttons — always visible regardless of active tab
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    game_state.pending_prim_save = Some(edit_dialog.clone());
                    edit_dialog.visible = false;
                    game_state.editing_prim_id = None;
                }

                if ui.button("Cancel").clicked() {
                    push_revert(&mut game_state, &edit_dialog);
                    edit_dialog.visible = false;
                    game_state.editing_prim_id = None;
                }

                if !edit_dialog.is_new && ui.button("Delete").clicked() {
                    if let Some(id) = edit_dialog.prim_id {
                        game_state.prims_to_delete.push(id);
                    }
                    edit_dialog.visible = false;
                    game_state.editing_prim_id = None;
                }
            });
        });

    if !dialog_open {
        push_revert(&mut game_state, &edit_dialog);
        edit_dialog.visible = false;
        edit_dialog.texture_picker_open = false;
        game_state.editing_prim_id = None;
    }

    // Texture picker window
    if edit_dialog.texture_picker_open && edit_dialog.visible {
        let mut picker_open = edit_dialog.texture_picker_open;
        let mut clicked_texture: Option<String> = None;
        Window::new("Texture Library")
            .open(&mut picker_open)
            .default_width(380.0)
            .show(egui.ctx_mut(), |ui| {
                egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                    let cols = 4usize;
                    egui::Grid::new("tex_grid")
                        .num_columns(cols)
                        .spacing([8.0, 8.0])
                        .show(ui, |ui| {
                            for (i, entry) in texture_lib.entries.iter().enumerate() {
                                let selected =
                                    edit_dialog.texture_id.as_deref() == Some(&entry.id);
                                let cell_clicked = ui
                                    .vertical(|ui| {
                                        let clicked = if let Some(handle) =
                                            texture_lib.egui_handles.get(&entry.id)
                                        {
                                            let sized = egui::load::SizedTexture::new(
                                                handle.id(),
                                                egui::vec2(72.0, 72.0),
                                            );
                                            let resp = ui.add(
                                                egui::Image::new(sized)
                                                    .sense(egui::Sense::click()),
                                            );
                                            if selected {
                                                ui.painter().rect_stroke(
                                                    resp.rect,
                                                    2.0,
                                                    egui::Stroke::new(
                                                        2.5_f32,
                                                        ui.visuals().selection.bg_fill,
                                                    ),
                                                    egui::StrokeKind::Outside,
                                                );
                                            }
                                            resp.clicked()
                                        } else {
                                            // Fallback text tile while egui handle is pending.
                                            ui.add_sized(
                                                [72.0, 72.0],
                                                egui::SelectableLabel::new(selected, &entry.name),
                                            )
                                            .clicked()
                                        };
                                        ui.label(
                                            egui::RichText::new(&entry.name)
                                                .small()
                                                .line_height(Some(14.0)),
                                        );
                                        clicked
                                    })
                                    .inner;
                                if cell_clicked {
                                    clicked_texture = Some(entry.id.clone());
                                }
                                if (i + 1) % cols == 0 {
                                    ui.end_row();
                                }
                            }
                            if !texture_lib.entries.is_empty()
                                && texture_lib.entries.len() % cols != 0
                            {
                                ui.end_row();
                            }
                        });
                });
            });
        if let Some(id) = clicked_texture {
            edit_dialog.texture_id = Some(id);
            picker_open = false;
        }
        edit_dialog.texture_picker_open = picker_open;
    }
}

/// Sync dialog values → prim entity every frame while the edit dialog is open.
/// This gives live preview; Save persists to DB, Cancel reverts via `pending_prim_revert`.
pub fn apply_live_prim_edits(
    mut commands: Commands,
    edit_dialog: Res<EditDialogState>,
    game_state: Res<GameState>,
    mut prim_query: Query<(
        Entity,
        &mut Prim,
        &mut Transform,
        Option<&MeshMaterial3d<StandardMaterial>>,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if !edit_dialog.visible || edit_dialog.is_new {
        return;
    }
    let Some(prim_id) = game_state.editing_prim_id else {
        return;
    };
    for (entity, mut prim, mut transform, mat_handle) in prim_query.iter_mut() {
        if prim.id != prim_id {
            continue;
        }
        transform.translation = Vec3::new(
            edit_dialog.position[0],
            edit_dialog.position[1],
            edit_dialog.position[2],
        );
        transform.rotation = Quat::from_euler(
            EulerRot::XYZ,
            edit_dialog.rotation[0],
            edit_dialog.rotation[1],
            edit_dialog.rotation[2],
        );
        transform.scale = Vec3::new(
            edit_dialog.scale[0],
            edit_dialog.scale[1],
            edit_dialog.scale[2],
        );
        let new_color =
            Color::srgb(edit_dialog.color[0], edit_dialog.color[1], edit_dialog.color[2]);
        prim.color = new_color;
        prim.name = edit_dialog.name.clone();

        // Detect geometry changes and request a mesh rebuild.
        let new_shape = PrimShape::from_str(&edit_dialog.shape);
        let geom_changed = new_shape != prim.shape
            || (edit_dialog.path_cut_begin - prim.path_cut_begin).abs() > 0.001
            || (edit_dialog.path_cut_end - prim.path_cut_end).abs() > 0.001
            || (edit_dialog.hollow - prim.hollow).abs() > 0.001
            || warp_differs(&edit_dialog.warp, &prim);

        if geom_changed {
            prim.shape = new_shape;
            prim.path_cut_begin = edit_dialog.path_cut_begin;
            prim.path_cut_end = edit_dialog.path_cut_end;
            prim.hollow = edit_dialog.hollow;
            apply_warp(&mut prim, &edit_dialog.warp);
            commands.entity(entity).insert(NeedsMeshRebuild);
        }

        // Update material directly — Selected doesn't change during editing so
        // highlight_selected_prim won't fire on its own.
        if let Some(handle) = mat_handle {
            if let Some(mat) = materials.get_mut(&handle.0) {
                // Live preview of the texture surface (transparency / glow / full-bright / UV),
                // then a subtle selection highlight on top so the surface stays visible.
                crate::systems::rendering::apply_surface(mat, new_color, &edit_dialog.surface);
                crate::systems::rendering::apply_selection_highlight(mat);
            }
        }
        break;
    }
}

fn push_revert(game_state: &mut GameState, dialog: &EditDialogState) {
    if !dialog.is_new {
        use crate::resources::EditDialogState;
        game_state.pending_prim_revert = Some(EditDialogState {
            prim_id: dialog.prim_id,
            name: dialog.original_name.clone(),
            shape: dialog.original_shape.clone(),
            position: dialog.original_position,
            rotation: dialog.original_rotation,
            scale: dialog.original_scale,
            color: dialog.original_color,
            original_texture_id: dialog.original_texture_id.clone(),
            path_cut_begin: dialog.original_path_cut_begin,
            path_cut_end: dialog.original_path_cut_end,
            hollow: dialog.original_hollow,
            warp: dialog.original_warp,
            surface: dialog.original_surface,
            ..Default::default()
        });
    }
}

/// True when any twist/taper/top-shear/slice value differs from the prim's.
fn warp_differs(w: &crate::resources::PrimWarp, prim: &Prim) -> bool {
    (w.twist_begin - prim.twist_begin).abs() > 0.001
        || (w.twist_end - prim.twist_end).abs() > 0.001
        || (w.taper_x - prim.taper_x).abs() > 0.001
        || (w.taper_y - prim.taper_y).abs() > 0.001
        || (w.top_shear_x - prim.top_shear_x).abs() > 0.001
        || (w.top_shear_y - prim.top_shear_y).abs() > 0.001
        || (w.slice_begin - prim.slice_begin).abs() > 0.001
        || (w.slice_end - prim.slice_end).abs() > 0.001
}

/// Copy the dialog's warp parameters onto a prim component.
fn apply_warp(prim: &mut Prim, w: &crate::resources::PrimWarp) {
    prim.twist_begin = w.twist_begin;
    prim.twist_end = w.twist_end;
    prim.taper_x = w.taper_x;
    prim.taper_y = w.taper_y;
    prim.top_shear_x = w.top_shear_x;
    prim.top_shear_y = w.top_shear_y;
    prim.slice_begin = w.slice_begin;
    prim.slice_end = w.slice_end;
}

/// Convert dialog warp (f32) into DB params (f64).
fn warp_params(w: &crate::resources::PrimWarp) -> crate::systems::prim_ops::WarpParams {
    crate::systems::prim_ops::WarpParams {
        twist_begin: w.twist_begin as f64,
        twist_end: w.twist_end as f64,
        taper_x: w.taper_x as f64,
        taper_y: w.taper_y as f64,
        top_shear_x: w.top_shear_x as f64,
        top_shear_y: w.top_shear_y as f64,
        slice_begin: w.slice_begin as f64,
        slice_end: w.slice_end as f64,
    }
}

/// Persist prim changes to the local database and update the Bevy world.
pub fn send_prim_mutations(
    mut commands: Commands,
    mut game_state: ResMut<GameState>,
    db: Option<Res<Database>>,
    mut prim_query: Query<(Entity, &mut Prim, &mut Transform, Option<&MeshMaterial3d<StandardMaterial>>)>,
    region_query: Query<&Region>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    texture_cache: Res<PrimTextureCache>,
    online: Option<Res<OnlineSession>>,
) {
    use crate::systems::prim_ops;

    if let Some(dialog_state) = game_state.pending_prim_save.take() {
        if dialog_state.is_new {
            let region_id = region_query.iter().next().map(|r| r.id).unwrap_or(1);

            // Online: ask the server to create the prim with its full initial state; the
            // PrimUpsert echo spawns it locally with the server-assigned id.
            if let Some(sess) = online.as_ref() {
                let _ = sess.intent_tx.send(NetMessage::CreatePrim {
                    request_id: 0,
                    region_id,
                    name: dialog_state.name.clone(),
                    position: Vec3::new(
                        dialog_state.position[0],
                        dialog_state.position[1],
                        dialog_state.position[2],
                    ),
                    rotation: Vec3::new(
                        dialog_state.rotation[0],
                        dialog_state.rotation[1],
                        dialog_state.rotation[2],
                    ),
                    scale: Vec3::new(
                        dialog_state.scale[0],
                        dialog_state.scale[1],
                        dialog_state.scale[2],
                    ),
                    color: dialog_state.color,
                    texture_id: dialog_state.texture_id.clone(),
                    shape: dialog_state.shape.clone(),
                    surface: dialog_state.surface,
                    geometry: dialog_state.geometry(),
                });
                tracing::info!(name = %dialog_state.name, "sent CreatePrim (online)");
                return;
            }

            let tex = dialog_state.texture_id.as_deref();

            let new_id: Option<i64> = db.as_ref().and_then(|db| {
                let conn = db.conn.lock().unwrap();
                prim_ops::db_create_prim(
                    &conn,
                    region_id,
                    &dialog_state.name,
                    &dialog_state.shape,
                    [
                        dialog_state.position[0] as f64,
                        dialog_state.position[1] as f64,
                        dialog_state.position[2] as f64,
                    ],
                    [
                        dialog_state.rotation[0] as f64,
                        dialog_state.rotation[1] as f64,
                        dialog_state.rotation[2] as f64,
                    ],
                    [
                        dialog_state.scale[0] as f64,
                        dialog_state.scale[1] as f64,
                        dialog_state.scale[2] as f64,
                    ],
                    [
                        dialog_state.color[0] as f64,
                        dialog_state.color[1] as f64,
                        dialog_state.color[2] as f64,
                    ],
                    tex,
                    dialog_state.path_cut_begin as f64,
                    dialog_state.path_cut_end as f64,
                    dialog_state.hollow as f64,
                    warp_params(&dialog_state.warp),
                    dialog_state.surface,
                )
                .ok()
            });

            if let Some(id) = new_id {
                commands.spawn((
                    Prim {
                        id,
                        region_id,
                        name: dialog_state.name.clone(),
                        shape: PrimShape::from_str(&dialog_state.shape),
                        color: Color::srgb(
                            dialog_state.color[0],
                            dialog_state.color[1],
                            dialog_state.color[2],
                        ),
                        texture_id: dialog_state.texture_id.clone(),
                        path_cut_begin: dialog_state.path_cut_begin,
                        path_cut_end: dialog_state.path_cut_end,
                        hollow: dialog_state.hollow,
                        twist_begin: dialog_state.warp.twist_begin,
                        twist_end: dialog_state.warp.twist_end,
                        taper_x: dialog_state.warp.taper_x,
                        taper_y: dialog_state.warp.taper_y,
                        top_shear_x: dialog_state.warp.top_shear_x,
                        top_shear_y: dialog_state.warp.top_shear_y,
                        slice_begin: dialog_state.warp.slice_begin,
                        slice_end: dialog_state.warp.slice_end,
                        surface: dialog_state.surface,
                    },
                    Transform::from_xyz(
                        dialog_state.position[0],
                        dialog_state.position[1],
                        dialog_state.position[2],
                    )
                    .with_rotation(Quat::from_euler(
                        EulerRot::XYZ,
                        dialog_state.rotation[0],
                        dialog_state.rotation[1],
                        dialog_state.rotation[2],
                    ))
                    .with_scale(Vec3::new(
                        dialog_state.scale[0],
                        dialog_state.scale[1],
                        dialog_state.scale[2],
                    )),
                    Selected,
                ));
                game_state.selected_prim_id = Some(id);
                tracing::info!(id, name = %dialog_state.name, "created prim");
            }
        } else if let Some(prim_id) = dialog_state.prim_id {
            // Online: send the authoritative update to the server. We still apply it
            // locally below for immediate feedback; the PrimUpsert echo is skipped for
            // the selected prim, so there's no fight.
            if let Some(sess) = online.as_ref() {
                let _ = sess.intent_tx.send(NetMessage::UpdatePrim {
                    request_id: 0,
                    prim_id,
                    position: Vec3::new(
                        dialog_state.position[0],
                        dialog_state.position[1],
                        dialog_state.position[2],
                    ),
                    rotation: Vec3::new(
                        dialog_state.rotation[0],
                        dialog_state.rotation[1],
                        dialog_state.rotation[2],
                    ),
                    scale: Vec3::new(
                        dialog_state.scale[0],
                        dialog_state.scale[1],
                        dialog_state.scale[2],
                    ),
                    color: dialog_state.color,
                    texture_id: dialog_state.texture_id.clone(),
                    name: dialog_state.name.clone(),
                    surface: dialog_state.surface,
                    geometry: dialog_state.geometry(),
                });
            }
            let tex = dialog_state.texture_id.as_deref();
            if let Some(ref db) = db {
                let conn = db.conn.lock().unwrap();
                let _ = prim_ops::db_update_prim(
                    &conn,
                    prim_id,
                    &dialog_state.name,
                    &dialog_state.shape,
                    [
                        dialog_state.position[0] as f64,
                        dialog_state.position[1] as f64,
                        dialog_state.position[2] as f64,
                    ],
                    [
                        dialog_state.rotation[0] as f64,
                        dialog_state.rotation[1] as f64,
                        dialog_state.rotation[2] as f64,
                    ],
                    [
                        dialog_state.scale[0] as f64,
                        dialog_state.scale[1] as f64,
                        dialog_state.scale[2] as f64,
                    ],
                    [
                        dialog_state.color[0] as f64,
                        dialog_state.color[1] as f64,
                        dialog_state.color[2] as f64,
                    ],
                    tex,
                    dialog_state.path_cut_begin as f64,
                    dialog_state.path_cut_end as f64,
                    dialog_state.hollow as f64,
                    warp_params(&dialog_state.warp),
                    dialog_state.surface,
                );
            }
            for (entity, mut prim, mut transform, mat_handle_opt) in prim_query.iter_mut() {
                if prim.id == prim_id {
                    let texture_changed = prim.texture_id != dialog_state.texture_id;
                    let geom_changed = prim.shape != PrimShape::from_str(&dialog_state.shape)
                        || (prim.path_cut_begin - dialog_state.path_cut_begin).abs() > 0.0001
                        || (prim.path_cut_end - dialog_state.path_cut_end).abs() > 0.0001
                        || (prim.hollow - dialog_state.hollow).abs() > 0.0001
                        || warp_differs(&dialog_state.warp, &prim);

                    prim.name = dialog_state.name.clone();
                    prim.shape = PrimShape::from_str(&dialog_state.shape);
                    prim.color = Color::srgb(
                        dialog_state.color[0],
                        dialog_state.color[1],
                        dialog_state.color[2],
                    );
                    prim.texture_id = dialog_state.texture_id.clone();
                    prim.path_cut_begin = dialog_state.path_cut_begin;
                    prim.path_cut_end = dialog_state.path_cut_end;
                    prim.hollow = dialog_state.hollow;
                    prim.surface = dialog_state.surface;
                    apply_warp(&mut prim, &dialog_state.warp);
                    *transform = Transform::from_xyz(
                        dialog_state.position[0],
                        dialog_state.position[1],
                        dialog_state.position[2],
                    )
                    .with_rotation(Quat::from_euler(
                        EulerRot::XYZ,
                        dialog_state.rotation[0],
                        dialog_state.rotation[1],
                        dialog_state.rotation[2],
                    ))
                    .with_scale(Vec3::new(
                        dialog_state.scale[0],
                        dialog_state.scale[1],
                        dialog_state.scale[2],
                    ));
                    if texture_changed {
                        apply_texture_to_material(
                            mat_handle_opt,
                            &mut materials,
                            &texture_cache,
                            dialog_state.texture_id.as_deref(),
                        );
                    }
                    apply_surface_to_material(
                        mat_handle_opt,
                        &mut materials,
                        prim.color,
                        &prim.surface,
                    );
                    if geom_changed {
                        commands.entity(entity).insert(NeedsMeshRebuild);
                    }
                    commands.entity(entity).insert(Selected);
                    game_state.selected_prim_id = Some(prim_id);
                    tracing::info!(id = prim_id, "updated prim");
                    break;
                }
            }
        }
    }

    if let Some(revert) = game_state.pending_prim_revert.take() {
        if let Some(prim_id) = revert.prim_id {
            for (entity, mut prim, mut transform, mat_handle_opt) in prim_query.iter_mut() {
                if prim.id == prim_id {
                    let texture_changed = prim.texture_id != revert.original_texture_id;
                    let geom_changed = prim.shape != PrimShape::from_str(&revert.shape)
                        || (prim.path_cut_begin - revert.path_cut_begin).abs() > 0.0001
                        || (prim.path_cut_end - revert.path_cut_end).abs() > 0.0001
                        || (prim.hollow - revert.hollow).abs() > 0.0001
                        || warp_differs(&revert.warp, &prim);

                    prim.name = revert.name.clone();
                    prim.shape = PrimShape::from_str(&revert.shape);
                    prim.color = Color::srgb(revert.color[0], revert.color[1], revert.color[2]);
                    prim.texture_id = revert.original_texture_id.clone();
                    prim.path_cut_begin = revert.path_cut_begin;
                    prim.path_cut_end = revert.path_cut_end;
                    prim.hollow = revert.hollow;
                    prim.surface = revert.surface;
                    apply_warp(&mut prim, &revert.warp);
                    *transform = Transform::from_xyz(
                        revert.position[0],
                        revert.position[1],
                        revert.position[2],
                    )
                    .with_rotation(Quat::from_euler(
                        EulerRot::XYZ,
                        revert.rotation[0],
                        revert.rotation[1],
                        revert.rotation[2],
                    ))
                    .with_scale(Vec3::new(revert.scale[0], revert.scale[1], revert.scale[2]));
                    if texture_changed {
                        apply_texture_to_material(
                            mat_handle_opt,
                            &mut materials,
                            &texture_cache,
                            revert.original_texture_id.as_deref(),
                        );
                    }
                    apply_surface_to_material(
                        mat_handle_opt,
                        &mut materials,
                        prim.color,
                        &prim.surface,
                    );
                    if geom_changed {
                        commands.entity(entity).insert(NeedsMeshRebuild);
                    }
                    commands.entity(entity).insert(Selected);
                    game_state.selected_prim_id = Some(prim_id);
                    break;
                }
            }
        }
    }

    for prim_id in std::mem::take(&mut game_state.prims_to_delete) {
        if let Some(sess) = online.as_ref() {
            let _ = sess
                .intent_tx
                .send(NetMessage::DeletePrim { request_id: 0, prim_id });
        }
        if let Some(ref db) = db {
            let conn = db.conn.lock().unwrap();
            let _ = conn.execute("DELETE FROM prims WHERE id=?1", rusqlite::params![prim_id]);
        }
        for (entity, prim, _, _) in prim_query.iter() {
            if prim.id == prim_id {
                commands.entity(entity).despawn();
                tracing::info!(id = prim_id, "deleted prim");
                break;
            }
        }
    }
}

/// Applies (or clears) a texture on a material handle in-place, same frame.
fn apply_texture_to_material(
    mat_handle_opt: Option<&MeshMaterial3d<StandardMaterial>>,
    materials: &mut Assets<StandardMaterial>,
    cache: &PrimTextureCache,
    texture_id: Option<&str>,
) {
    let Some(mat_handle) = mat_handle_opt else {
        return;
    };
    let Some(mat) = materials.get_mut(&mat_handle.0) else {
        return;
    };
    mat.base_color_texture = texture_id
        .and_then(|id| cache.handles.get(id))
        .cloned()
        .map(|h| h.into());
}

/// Applies a prim's surface params (transparency / glow / full-bright / UV transform)
/// onto a material handle in-place, preserving its `base_color_texture`.
fn apply_surface_to_material(
    mat_handle_opt: Option<&MeshMaterial3d<StandardMaterial>>,
    materials: &mut Assets<StandardMaterial>,
    color: Color,
    surface: &vibe_core::PrimSurface,
) {
    let Some(mat_handle) = mat_handle_opt else {
        return;
    };
    let Some(mat) = materials.get_mut(&mat_handle.0) else {
        return;
    };
    crate::systems::rendering::apply_surface(mat, color, surface);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const SHAPES: &[(&str, &str)] = &[
    ("box", "Box"),
    ("sphere", "Sphere"),
    ("cylinder", "Cylinder"),
    ("cone", "Cone"),
];

fn shape_display_name(key: &str) -> &str {
    SHAPES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, label)| *label)
        .unwrap_or(key)
}
