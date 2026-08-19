//! Transform gizmo system (Phase 6 - ADR-017).
//!
//! Second Life–style axis handles for the selected prim.
//! A drag only starts when the cursor is clicked directly on an axis handle (within 16 px).
//! Mode toolbar (top-left) shows [Move] [Rotate] [Scale]; T/R/S keys also switch mode.

use crate::components::{Prim, Selected};
use crate::resources::{EditDialogState, OnlineSession};
use crate::systems::egui_manager::EguiManager;
use bevy::prelude::*;
use big_space::prelude::GridCell;
use cartopolis_core::NetMessage;
use std::f32::consts::TAU;

// ─── Types ────────────────────────────────────────────────────────────────────

#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoMode {
    #[default]
    Translate,
    Rotate,
    Scale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoAxis {
    X,
    Y,
    Z,
}

#[derive(Resource, Default)]
pub struct GizmoState {
    pub mode: GizmoMode,
    /// Which axis handle is being dragged; None = no drag in progress.
    pub active_axis: Option<GizmoAxis>,
    /// Screen-space cursor position at drag start.
    pub drag_start_screen: Vec2,
    /// Copy of the prim transform when the drag started (used as the drag base).
    pub drag_start_transform: Option<Transform>,
    pub last_update_time: f32,
}

// ─── Keyboard mode switch ─────────────────────────────────────────────────────

pub fn handle_gizmo_mode_input(
    mut gizmo_state: ResMut<GizmoState>,
    keyboard: Res<ButtonInput<KeyCode>>,
    egui_manager: Res<EguiManager>,
    selected: Query<(), With<Selected>>,
) {
    // Ignore T/R/S when a text field is focused or a Ctrl chord is held; stays live
    // while the edit dialog is open (pass `false`) so mode switching works during editing.
    if egui_manager.ui_owns_keyboard(false, &keyboard) {
        return;
    }
    // …and only while something is actually selected, because two of these three
    // keys belong to the avatar as well: `R` is the flight climb key and `S` walks
    // backwards. Without this the gizmo mode silently changed under you every time
    // you walked, and the mode you next selected an object with was whichever
    // direction you had last moved in. The gizmo is only meaningful with a
    // selection, so gating on one costs nothing and ends the collision.
    if selected.is_empty() {
        return;
    }
    if keyboard.just_pressed(KeyCode::KeyT) {
        gizmo_state.mode = GizmoMode::Translate;
    } else if keyboard.just_pressed(KeyCode::KeyR) {
        gizmo_state.mode = GizmoMode::Rotate;
    } else if keyboard.just_pressed(KeyCode::KeyS) {
        gizmo_state.mode = GizmoMode::Scale;
    }
}

// ─── Visual handles ───────────────────────────────────────────────────────────

pub fn render_gizmo_visuals(
    selected_query: Query<&GlobalTransform, With<Selected>>,
    gizmo_state: Res<GizmoState>,
    mut gizmos: Gizmos,
) {
    for gt in selected_query.iter() {
        let pos = gt.translation();
        let arm = arm_length(gt.scale().max_element());

        let cx = axis_color(GizmoAxis::X, &gizmo_state);
        let cy = axis_color(GizmoAxis::Y, &gizmo_state);
        let cz = axis_color(GizmoAxis::Z, &gizmo_state);

        match gizmo_state.mode {
            GizmoMode::Translate => {
                gizmos.arrow(pos, pos + Vec3::X * arm, cx);
                gizmos.arrow(pos, pos + Vec3::Y * arm, cy);
                gizmos.arrow(pos, pos + Vec3::Z * arm, cz);
            }
            GizmoMode::Rotate => {
                // Each ring lies in the plane perpendicular to its axis.
                gizmos.circle(
                    Isometry3d::new(pos, Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)),
                    arm,
                    cx, // X-axis ring (YZ plane)
                );
                gizmos.circle(
                    Isometry3d::new(pos, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
                    arm,
                    cy, // Y-axis ring (XZ plane)
                );
                gizmos.circle(Isometry3d::new(pos, Quat::IDENTITY), arm, cz); // Z-axis ring (XY plane)
            }
            GizmoMode::Scale => {
                // Bidirectional stubs with a small cross-cap at each tip.
                for (axis, col) in [(Vec3::X, cx), (Vec3::Y, cy), (Vec3::Z, cz)] {
                    let tip = pos + axis * arm;
                    gizmos.line(pos, tip, col);
                    // Small perpendicular cross to distinguish from translate arrows.
                    let perp = if axis.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
                    let cross_half = arm * 0.12;
                    let t1 = perp * cross_half;
                    let t2 = axis.cross(perp).normalize() * cross_half;
                    gizmos.line(tip - t1, tip + t1, col);
                    gizmos.line(tip - t2, tip + t2, col);
                }
            }
        }
    }
}

fn arm_length(max_scale: f32) -> f32 {
    (max_scale * 0.8).max(1.2)
}

fn axis_color(axis: GizmoAxis, state: &GizmoState) -> Color {
    if state.active_axis == Some(axis) {
        return Color::WHITE;
    }
    match axis {
        GizmoAxis::X => Color::srgb(1.0, 0.2, 0.2),
        GizmoAxis::Y => Color::srgb(0.2, 1.0, 0.2),
        GizmoAxis::Z => Color::srgb(0.2, 0.2, 1.0),
    }
}

fn axis_vec(axis: GizmoAxis) -> Vec3 {
    match axis {
        GizmoAxis::X => Vec3::X,
        GizmoAxis::Y => Vec3::Y,
        GizmoAxis::Z => Vec3::Z,
    }
}

// ─── Axis hit-test ────────────────────────────────────────────────────────────

const HIT_THRESHOLD_PX: f32 = 16.0;

/// Returns the axis handle closest to the cursor, dispatching to tip-test (translate/scale)
/// or ring-test (rotate).
fn find_hit_axis(
    camera: &Camera,
    cam_gt: &GlobalTransform,
    prim_pos: Vec3,
    arm: f32,
    cursor: Vec2,
    mode: GizmoMode,
) -> Option<GizmoAxis> {
    match mode {
        GizmoMode::Translate | GizmoMode::Scale => {
            find_hit_axis_tip(camera, cam_gt, prim_pos, arm, cursor)
        }
        GizmoMode::Rotate => find_hit_axis_ring(camera, cam_gt, prim_pos, arm, cursor),
    }
}

/// Hit-test arrow / scale-stub tips (single point per axis).
fn find_hit_axis_tip(
    camera: &Camera,
    cam_gt: &GlobalTransform,
    prim_pos: Vec3,
    arm: f32,
    cursor: Vec2,
) -> Option<GizmoAxis> {
    let mut best: Option<GizmoAxis> = None;
    let mut best_dist = HIT_THRESHOLD_PX;

    for axis in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
        let tip_world = prim_pos + axis_vec(axis) * arm;
        if let Ok(tip_screen) = camera.world_to_viewport(cam_gt, tip_world) {
            let dist = (tip_screen - cursor).length();
            if dist < best_dist {
                best_dist = dist;
                best = Some(axis);
            }
        }
    }
    best
}

/// Hit-test rotation rings by sampling 32 points along each ring circumference.
fn find_hit_axis_ring(
    camera: &Camera,
    cam_gt: &GlobalTransform,
    prim_pos: Vec3,
    arm: f32,
    cursor: Vec2,
) -> Option<GizmoAxis> {
    // (axis, tangent_u, tangent_v) — ring lies in the plane spanned by u and v.
    let rings: [(GizmoAxis, Vec3, Vec3); 3] = [
        (GizmoAxis::X, Vec3::Y, Vec3::Z), // YZ plane  → rotate around X
        (GizmoAxis::Y, Vec3::X, Vec3::Z), // XZ plane  → rotate around Y
        (GizmoAxis::Z, Vec3::X, Vec3::Y), // XY plane  → rotate around Z
    ];

    const N: usize = 32;
    let mut best: Option<GizmoAxis> = None;
    let mut best_dist = HIT_THRESHOLD_PX;

    for (axis, u, v) in rings {
        for i in 0..N {
            let angle = i as f32 * TAU / N as f32;
            let pt = prim_pos + (u * angle.cos() + v * angle.sin()) * arm;
            if let Ok(screen) = camera.world_to_viewport(cam_gt, pt) {
                let dist = (screen - cursor).length();
                if dist < best_dist {
                    best_dist = dist;
                    best = Some(axis);
                }
            }
        }
    }
    best
}

// ─── Drag interaction ─────────────────────────────────────────────────────────

pub fn handle_gizmo_interaction(
    // ADR-029: `Option<&GridCell>` lets the send path reconstruct the prim's
    // absolute world position (cell + local) to persist as a geo anchor.
    mut selected_query: Query<(&mut Transform, &Prim, Option<&GridCell>), With<Selected>>,
    mut gizmo_state: ResMut<GizmoState>,
    mut edit_dialog: ResMut<EditDialogState>,
    egui_manager: Res<EguiManager>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform)>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    online_session: Option<Res<OnlineSession>>,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    if selected_query.is_empty() {
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
    let (camera, cam_gt) = match cameras.iter().next() {
        Some(c) => c,
        None => return,
    };

    // ── On press: hit-test axis handles (skip if egui is consuming the click) ─
    if mouse_buttons.just_pressed(MouseButton::Left) && !egui_manager.ctx.egui_wants_pointer_input()
    {
        // Read prim position without a mutable borrow.
        let (prim_pos, arm, start_tr) = {
            let Ok((tr, _, _)) = selected_query.single() else {
                return;
            };
            (tr.translation, arm_length(tr.scale.max_element()), *tr)
        };

        if let Some(axis) = find_hit_axis(camera, cam_gt, prim_pos, arm, cursor, gizmo_state.mode) {
            gizmo_state.active_axis = Some(axis);
            gizmo_state.drag_start_screen = cursor;
            gizmo_state.drag_start_transform = Some(start_tr);
        }
        // No axis hit → do nothing; prim_picking handles selection normally.
    }

    // ── On release: clear drag state, send final network update ──────────────
    if mouse_buttons.just_released(MouseButton::Left) {
        if gizmo_state.active_axis.is_some() {
            let Ok((transform, prim, cell)) = selected_query.single() else {
                return;
            };
            if let Some(ref session) = online_session {
                // ADR-029: persist the new position as an absolute geo anchor.
                if let Some(geo) = crate::systems::prim_geo::geo_for(cell, transform, &map_anchor) {
                    let (rx, ry, rz) = transform.rotation.to_euler(EulerRot::XYZ);
                    session.send(
                        "update prim",
                        NetMessage::UpdatePrim {
                            request_id: generate_request_id(),
                            prim_id: prim.id,
                            position: transform.translation,
                            rotation: Vec3::new(rx, ry, rz),
                            scale: transform.scale,
                            color: edit_dialog.color,
                            texture_id: edit_dialog.texture_id.clone(),
                            name: edit_dialog.name.clone(),
                            surface: edit_dialog.surface,
                            geometry: edit_dialog.geometry(),
                            geo,
                        },
                    );
                }
            }
            gizmo_state.active_axis = None;
            gizmo_state.drag_start_transform = None;
        }
    }

    // ── Drag: apply axis-constrained transform ────────────────────────────────
    let Some(active_axis) = gizmo_state.active_axis else {
        return;
    };
    let Some(start_tr) = gizmo_state.drag_start_transform else {
        return;
    };

    let mouse_delta = cursor - gizmo_state.drag_start_screen;
    let dir = axis_vec(active_axis);
    let arm = arm_length(start_tr.scale.max_element());

    // Project the axis into screen space to get a pixels-per-world-unit ratio.
    let Ok(ss_origin) = camera.world_to_viewport(cam_gt, start_tr.translation) else {
        return;
    };
    let Ok(ss_tip) = camera.world_to_viewport(cam_gt, start_tr.translation + dir * arm) else {
        return;
    };

    let screen_axis = ss_tip - ss_origin;
    let screen_arm_px = screen_axis.length();
    if screen_arm_px < 0.1 {
        return;
    }
    let screen_norm = screen_axis / screen_arm_px;
    // Signed distance the cursor moved along this axis (in world units).
    let world_delta = screen_norm.dot(mouse_delta) / screen_arm_px * arm;

    let Ok((mut transform, prim, cell)) = selected_query.single_mut() else {
        return;
    };

    match gizmo_state.mode {
        GizmoMode::Translate => {
            transform.translation = start_tr.translation + dir * world_delta;
        }
        GizmoMode::Rotate => {
            let angle = screen_norm.dot(mouse_delta) * 0.02; // ~1° per 1.8 px
            transform.rotation = Quat::from_axis_angle(dir, angle) * start_tr.rotation;
        }
        GizmoMode::Scale => {
            let factor = (1.0 + world_delta * 0.5).max(0.05);
            let scale_mul = match active_axis {
                GizmoAxis::X => Vec3::new(factor, 1.0, 1.0),
                GizmoAxis::Y => Vec3::new(1.0, factor, 1.0),
                GizmoAxis::Z => Vec3::new(1.0, 1.0, factor),
            };
            transform.scale = start_tr.scale * scale_mul;
        }
    }

    // Keep the edit dialog in sync.
    let pos = transform.translation;
    let (rx, ry, rz) = transform.rotation.to_euler(EulerRot::XYZ);
    edit_dialog.position = [pos.x, pos.y, pos.z];
    edit_dialog.rotation = [rx, ry, rz];
    edit_dialog.scale = [transform.scale.x, transform.scale.y, transform.scale.z];

    // Throttle network updates to ~20 Hz.
    gizmo_state.last_update_time += time.delta_secs();
    if gizmo_state.last_update_time >= 0.05 {
        gizmo_state.last_update_time = 0.0;
        if let Some(ref session) = online_session {
            // ADR-029: reproject the dragged position back to an absolute geo anchor.
            if let Some(geo) = crate::systems::prim_geo::geo_for(cell, &transform, &map_anchor) {
                session.send(
                    "update prim",
                    NetMessage::UpdatePrim {
                        request_id: generate_request_id(),
                        prim_id: prim.id,
                        position: transform.translation,
                        rotation: Vec3::new(rx, ry, rz),
                        scale: transform.scale,
                        color: edit_dialog.color,
                        texture_id: edit_dialog.texture_id.clone(),
                        name: edit_dialog.name.clone(),
                        surface: edit_dialog.surface,
                        geometry: edit_dialog.geometry(),
                        geo,
                    },
                );
            }
        }
    }
}

fn generate_request_id() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
