//! Prim selection via raycasting (Phase 4 - ADR-017).
//!
//! Left-click: single select (replaces selection).
//! Shift+left-click: toggle individual prim selection.
//! Left-drag on empty space: rubber-band (marquee) multi-select.
//! Shift+drag: add marquee hits to existing selection.
//! Delete / Backspace: delete all selected prims.

use crate::components::{Prim, Selected};
use crate::resources::{ContextMenuState, GameState, MarqueeState};
use crate::systems::egui_manager::EguiManager;
use crate::systems::rendering::PrimMesh;
use bevy::prelude::*;

const MARQUEE_DRAG_THRESHOLD_PX: f32 = 4.0;

// ─── Primary picking ─────────────────────────────────────────────────────────

/// System for handling prim picking and selection via raycasting.
pub fn prim_picking(
    mut commands: Commands,
    mut game_state: ResMut<GameState>,
    mut context_menu: ResMut<ContextMenuState>,
    mut marquee: ResMut<MarqueeState>,
    egui_manager: Res<EguiManager>,
    keyboard: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform)>,
    prim_query: Query<(Entity, &Prim, &GlobalTransform), With<PrimMesh>>,
    region_query: Query<(Entity, &crate::components::Region, &GlobalTransform), Without<Prim>>,
    selected_query: Query<Entity, With<Selected>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    gizmo_state: Res<crate::systems::gizmo::GizmoState>,
) {
    // Don't process world clicks when egui has the pointer (toolbar buttons, dialogs, etc.).
    if egui_manager.ctx.wants_pointer_input() {
        return;
    }

    // Any click outside egui closes the context menu.
    if context_menu.visible
        && (mouse_buttons.just_pressed(MouseButton::Left)
            || mouse_buttons.just_pressed(MouseButton::Right))
    {
        context_menu.visible = false;
    }

    let window = match windows.iter().next() {
        Some(w) => w,
        None => return,
    };

    let cursor_position = match window.cursor_position() {
        Some(pos) => pos,
        None => return,
    };

    let (camera, camera_global_transform) = match cameras.iter().next() {
        Some(cam) => cam,
        None => return,
    };

    // Proper perspective ray through the cursor position.
    let Ok(ray) = camera.viewport_to_world(camera_global_transform, cursor_position) else {
        return;
    };

    let ray_origin = ray.origin;
    let ray_dir = ray.direction.as_vec3();

    // Ray vs oriented-box hit test. The base prim mesh is a unit shape (±0.5 in local
    // space) scaled and rotated by its transform, so we transform the ray into each
    // prim's local space and intersect the unit box. The intersection parameter `t` is
    // shared with the world ray (affine map preserves it), and the world ray direction
    // is unit, so `t` is the true world distance to the surface the ray strikes. We keep
    // the prim with the nearest `t` — i.e. the surface actually under the cursor, not
    // whichever bounding volume's centre happens to sit closest to the camera.
    let mut hit_prim: Option<(Entity, &Prim, f32)> = None;
    for (entity, prim, gt) in prim_query.iter() {
        let inv = gt.affine().inverse();
        let local_origin = inv.transform_point3(ray_origin);
        let local_dir = inv.transform_vector3(ray_dir);
        if let Some(t) = ray_unit_box(local_origin, local_dir) {
            match hit_prim {
                Some((_, _, best)) if t >= best => {}
                _ => hit_prim = Some((entity, prim, t)),
            }
        }
    }

    let shift = keyboard.pressed(KeyCode::ShiftLeft) || keyboard.pressed(KeyCode::ShiftRight);

    // Left-click: select / deselect / start marquee
    if mouse_buttons.just_pressed(MouseButton::Left) {
        if gizmo_state.active_axis.is_some() {
            // A gizmo handle drag started on this click (it runs first) — keep the
            // current selection instead of treating the arm click as a world click.
        } else if let Some((entity, prim, _)) = hit_prim {
            if shift {
                // Shift+click: toggle this prim in the selection.
                if selected_query.get(entity).is_ok() {
                    commands.entity(entity).remove::<Selected>();
                    if game_state.selected_prim_id == Some(prim.id) {
                        game_state.selected_prim_id = None;
                    }
                } else {
                    commands.entity(entity).insert(Selected);
                    game_state.selected_prim_id = Some(prim.id);
                }
            } else {
                // Plain click: replace selection with this prim.
                for e in selected_query.iter() {
                    commands.entity(e).remove::<Selected>();
                }
                commands.entity(entity).insert(Selected);
                game_state.selected_prim_id = Some(prim.id);
                tracing::debug!(prim_id = prim.id, "prim selected");
            }
        } else {
            let ctrl =
                keyboard.pressed(KeyCode::ControlLeft) || keyboard.pressed(KeyCode::ControlRight);
            if ctrl {
                // Ctrl+drag on empty space → marquee; update_marquee handles the rest.
                marquee.press_pos = Some(cursor_position);
                marquee.shift_add = shift;
            } else if !shift {
                // Plain click on empty space → deselect all.
                for e in selected_query.iter() {
                    commands.entity(e).remove::<Selected>();
                }
                game_state.selected_prim_id = None;
                tracing::debug!("all prims deselected");
            }
            // Shift+click on empty: preserve existing selection.
        }
    }

    // Right-click: open context menu.
    if mouse_buttons.just_pressed(MouseButton::Right) {
        context_menu.visible = true;
        context_menu.screen_pos = cursor_position;

        if let Some((_, prim, _)) = hit_prim {
            context_menu.hit_prim_id = Some(prim.id);
            context_menu.hit_region_id = Some(prim.region_id);
            context_menu.hit_point = ray_plane_y(ray_origin, ray_dir, 0.0)
                .unwrap_or_else(|| ray_origin + ray_dir * 10.0);
            tracing::debug!(prim_id = prim.id, "context menu for prim");
        } else {
            context_menu.hit_prim_id = None;

            if let Some((_, region, region_transform)) = region_query.iter().next() {
                let region_y = region_transform.translation().y;
                context_menu.hit_region_id = Some(region.id);
                context_menu.hit_point = ray_plane_y(ray_origin, ray_dir, region_y)
                    .unwrap_or_else(|| ray_origin + ray_dir * 20.0);
            } else {
                context_menu.visible = false;
            }
            tracing::debug!("context menu for region");
        }
    }
}

// ─── Marquee update ───────────────────────────────────────────────────────────

/// Tracks the marquee drag each frame and finalises the selection on mouse release.
pub fn update_marquee(
    mut commands: Commands,
    mut marquee: ResMut<MarqueeState>,
    mut game_state: ResMut<GameState>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform)>,
    prim_query: Query<(Entity, &Prim, &GlobalTransform), With<PrimMesh>>,
    selected_query: Query<Entity, With<Selected>>,
    egui_manager: Res<EguiManager>,
) {
    if egui_manager.ctx.wants_pointer_input() {
        marquee.press_pos = None;
        marquee.active = false;
        return;
    }

    let window = match windows.iter().next() {
        Some(w) => w,
        None => return,
    };
    let cursor = match window.cursor_position() {
        Some(p) => p,
        None => return,
    };

    if mouse_buttons.pressed(MouseButton::Left) {
        if let Some(press_pos) = marquee.press_pos {
            if !marquee.active && (cursor - press_pos).length() > MARQUEE_DRAG_THRESHOLD_PX {
                marquee.active = true;
                marquee.start = press_pos;
            }
            if marquee.active {
                marquee.end = cursor;
            }
        }
        return;
    }

    // Mouse released — finalise.
    if mouse_buttons.just_released(MouseButton::Left) {
        if marquee.active {
            let (camera, cam_gt) = match cameras.iter().next() {
                Some(c) => c,
                None => {
                    reset_marquee(&mut marquee);
                    return;
                }
            };

            let lo = marquee.start.min(marquee.end);
            let hi = marquee.start.max(marquee.end);

            if !marquee.shift_add {
                for entity in selected_query.iter() {
                    commands.entity(entity).remove::<Selected>();
                }
                game_state.selected_prim_id = None;
            }

            for (entity, prim, gt) in prim_query.iter() {
                if let Ok(screen_pos) = camera.world_to_viewport(cam_gt, gt.translation()) {
                    if screen_pos.x >= lo.x
                        && screen_pos.x <= hi.x
                        && screen_pos.y >= lo.y
                        && screen_pos.y <= hi.y
                    {
                        commands.entity(entity).insert(Selected);
                        game_state.selected_prim_id = Some(prim.id);
                        tracing::debug!(prim_id = prim.id, "prim marquee-selected");
                    }
                }
            }
        }

        reset_marquee(&mut marquee);
    }
}

fn reset_marquee(m: &mut MarqueeState) {
    m.press_pos = None;
    m.active = false;
    m.start = Vec2::ZERO;
    m.end = Vec2::ZERO;
}

// ─── Marquee rendering ────────────────────────────────────────────────────────

/// Draws the rubber-band rectangle as an egui overlay.
pub fn render_marquee(mut egui: ResMut<EguiManager>, marquee: Res<MarqueeState>) {
    if !marquee.active {
        return;
    }

    let ctx = egui.ctx_mut();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("marquee_rect"),
    ));

    let min = egui::pos2(
        marquee.start.x.min(marquee.end.x),
        marquee.start.y.min(marquee.end.y),
    );
    let max = egui::pos2(
        marquee.start.x.max(marquee.end.x),
        marquee.start.y.max(marquee.end.y),
    );
    let rect = egui::Rect::from_min_max(min, max);

    let fill = egui::Color32::from_rgba_premultiplied(100, 160, 255, 25);
    let stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(140, 190, 255, 200));
    painter.rect(rect, egui::CornerRadius::default(), fill, stroke, egui::StrokeKind::Outside);
}

// ─── Delete key ──────────────────────────────────────────────────────────────

/// Pressing Delete (or Backspace) while prims are selected queues them for deletion.
/// Ignored when a text field has keyboard focus.
pub fn handle_delete_key(
    mut game_state: ResMut<GameState>,
    keyboard: Res<ButtonInput<KeyCode>>,
    egui_manager: Res<EguiManager>,
    selected_query: Query<&Prim, With<Selected>>,
) {
    if egui_manager.ctx.wants_keyboard_input() {
        return;
    }

    let delete_pressed =
        keyboard.just_pressed(KeyCode::Delete) || keyboard.just_pressed(KeyCode::Backspace);
    if !delete_pressed {
        return;
    }

    for prim in selected_query.iter() {
        game_state.prims_to_delete.push(prim.id);
        tracing::info!(prim_id = prim.id, "queued for deletion via keyboard");
    }
}

// ─── Highlight / unhighlight ──────────────────────────────────────────────────

/// System to highlight selected prim with a different material color
pub fn highlight_selected_prim(
    mut materials: ResMut<Assets<StandardMaterial>>,
    selected_query: Query<
        (&Prim, &MeshMaterial3d<StandardMaterial>),
        (With<Selected>, Changed<Selected>),
    >,
) {
    for (prim, material_handle) in selected_query.iter() {
        if let Some(material) = materials.get_mut(&material_handle.0) {
            // Keep the real surface (color/texture/alpha/glow/uv) and add only a subtle
            // highlight, so the texture stays visible while the prim is selected.
            crate::systems::rendering::apply_surface(material, prim.color, &prim.surface);
            crate::systems::rendering::apply_selection_highlight(material);
        }
    }
}

/// System to restore original color when prim is deselected
pub fn unhighlight_deselected_prim(
    mut materials: ResMut<Assets<StandardMaterial>>,
    // No With<Selected> filter: entity has already lost the component by the time
    // RemovedComponents fires.
    query: Query<(&Prim, &MeshMaterial3d<StandardMaterial>)>,
    mut deselected: RemovedComponents<Selected>,
) {
    for entity in deselected.read() {
        if let Ok((prim, material_handle)) = query.get(entity) {
            if let Some(material) = materials.get_mut(&material_handle.0) {
                // Restore the full surface (alpha/glow/full-bright/uv), not just RGB.
                crate::systems::rendering::apply_surface(material, prim.color, &prim.surface);
            }
        }
    }
}

// ─── Math helpers ─────────────────────────────────────────────────────────────

/// Intersect ray with horizontal plane at the given y level.
/// Returns None if the ray is nearly parallel to the plane or points away from it.
fn ray_plane_y(origin: Vec3, dir: Vec3, y: f32) -> Option<Vec3> {
    if dir.y.abs() < 1e-6 {
        return None;
    }
    let t = (y - origin.y) / dir.y;
    if t <= 0.0 {
        return None;
    }
    Some(origin + dir * t)
}

/// Ray vs the unit box `[-0.5, 0.5]³` (slab method). `o`/`d` are the ray in the box's
/// local space. Returns the nearest non-negative entry distance `t` (the parameter
/// `o + t·d`), or `None` if the ray misses. When the ray starts inside the box, the
/// exit distance is returned so an enclosing prim can still be picked.
fn ray_unit_box(o: Vec3, d: Vec3) -> Option<f32> {
    const H: f32 = 0.5;
    let mut t_enter = f32::NEG_INFINITY;
    let mut t_exit = f32::INFINITY;
    for i in 0..3 {
        let (oi, di) = (o[i], d[i]);
        if di.abs() < 1e-8 {
            // Ray parallel to this slab: origin must already lie within it.
            if oi < -H || oi > H {
                return None;
            }
        } else {
            let inv = 1.0 / di;
            let mut t1 = (-H - oi) * inv;
            let mut t2 = (H - oi) * inv;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            t_enter = t_enter.max(t1);
            t_exit = t_exit.min(t2);
            if t_enter > t_exit {
                return None;
            }
        }
    }
    if t_exit < 0.0 {
        return None; // box is entirely behind the ray
    }
    Some(if t_enter >= 0.0 { t_enter } else { t_exit })
}
