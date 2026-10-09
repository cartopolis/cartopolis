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
    /// Where the handles stood at drag start, in render space — what the screen
    /// projection of the dragged axis is measured from (see [`handle_frame`]).
    pub drag_start_origin: Vec3,
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
        let (pos, arm) = handle_frame(gt);

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

/// Clearance between the prim's bounds and a handle's tip, as a fraction of the
/// prim's bounding radius, and never less than [`MIN_ARM_CLEARANCE_M`].
const ARM_CLEARANCE: f32 = 0.5;
const MIN_ARM_CLEARANCE_M: f32 = 0.75;

/// How far a handle reaches from the prim's centre: past its **bounding sphere**,
/// plus clearance. Every prim mesh is a unit shape scaled by `scale`, so that
/// sphere's radius is half the scale vector's length, and it holds at any
/// rotation — the handles run along world axes, the prim need not.
///
/// It was `0.8 × max(scale)`, which is inside a cube's own corners and barely
/// clear of its faces: the tips sat on or behind the surface, the depth test hid
/// most of each arrow, and the point a click has to land on was under the prim.
fn arm_length(scale: Vec3) -> f32 {
    let radius = scale.abs().length() * 0.5;
    radius + (radius * ARM_CLEARANCE).max(MIN_ARM_CLEARANCE_M)
}

/// Where the handles of a selected prim stand and how long their arms are, in
/// **render space** — the one answer the drawing, the click test and the drag
/// all use.
///
/// From the `GlobalTransform`, never the `Transform`: a prim's `Transform` is an
/// offset inside its own `big_space` grid cell, while the camera projects render
/// space (relative to the camera's cell). The click test used to read the
/// `Transform`, which is the same point only while the prim and the camera share
/// a cell; across a cell boundary every handle was clickable ten kilometres from
/// where it was drawn.
fn handle_frame(gt: &GlobalTransform) -> (Vec3, f32) {
    (gt.translation(), arm_length(gt.scale()))
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
    mut selected_query: Query<
        (&mut Transform, &GlobalTransform, &Prim, Option<&GridCell>),
        With<Selected>,
    >,
    mut gizmo_state: ResMut<GizmoState>,
    mut edit_dialog: ResMut<EditDialogState>,
    egui_manager: Res<EguiManager>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<crate::components::FreeCamera>>,
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
    let Ok((camera, cam_gt)) = cameras.single() else {
        return;
    };

    // ── On press: hit-test axis handles (skip if egui is consuming the click) ─
    if mouse_buttons.just_pressed(MouseButton::Left) && !egui_manager.ctx.egui_wants_pointer_input()
    {
        // Read prim position without a mutable borrow.
        let (prim_pos, arm, start_tr) = {
            let Ok((tr, gt, _, _)) = selected_query.single() else {
                return;
            };
            let (pos, arm) = handle_frame(gt);
            (pos, arm, *tr)
        };

        if let Some(axis) = find_hit_axis(camera, cam_gt, prim_pos, arm, cursor, gizmo_state.mode) {
            gizmo_state.active_axis = Some(axis);
            gizmo_state.drag_start_screen = cursor;
            gizmo_state.drag_start_transform = Some(start_tr);
            gizmo_state.drag_start_origin = prim_pos;
        }
        // No axis hit → do nothing; prim_picking handles selection normally.
    }

    // ── On release: clear drag state, send final network update ──────────────
    if mouse_buttons.just_released(MouseButton::Left) {
        if gizmo_state.active_axis.is_some() {
            let Ok((transform, _, prim, cell)) = selected_query.single() else {
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
                            // Passed through, never invented: the gizmo moves a
                            // prim and knows nothing about its emitter, and an
                            // `UpdatePrim` is a *whole* state — sending `None`
                            // here would put out a fire every time somebody
                            // dragged the thing carrying it.
                            particles: prim.particles,
                            // The same argument, and the same source: the model is
                            // read off the prim, not off the dialog, because the
                            // gizmo can be dragging something the dialog was never
                            // opened on.
                            model: prim.model.clone(),
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
    let arm = arm_length(start_tr.scale);
    let origin = gizmo_state.drag_start_origin;

    // Project the axis into screen space to get a pixels-per-world-unit ratio.
    // From the render-space origin the handles were drawn at — the `Transform`
    // is cell-local and projects somewhere else entirely across a cell boundary.
    let Ok(ss_origin) = camera.world_to_viewport(cam_gt, origin) else {
        return;
    };
    let Ok(ss_tip) = camera.world_to_viewport(cam_gt, origin + dir * arm) else {
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

    let Ok((mut transform, _, prim, cell)) = selected_query.single_mut() else {
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
                        particles: prim.particles,
                        model: prim.model.clone(),
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

// ─── Readout for the shot harness ─────────────────────────────────────────────

/// What `--dump-state` says about the gizmo (`gizmo_render`, `gizmo_selected`,
/// `gizmo_axes_hit`), and what `--ui gizmo` exists to exercise.
///
/// Both halves of "the editing handles work" had broken without a word: the
/// handles were never drawn (the renderer was not compiled in, see
/// [`GizmoReadout::renders`]), and the click test measured them from a different
/// point than the one they were drawn at. A capture shows the first and nothing
/// shows the second, so the second is measured: every handle's drawn point is
/// projected to the screen and handed to the same hit test a click goes through,
/// which has to name that handle back.
#[derive(bevy::ecs::system::SystemParam)]
pub struct GizmoReadout<'w, 's> {
    selected: Query<'w, 's, &'static GlobalTransform, With<Selected>>,
    camera: Query<
        'w,
        's,
        (&'static Camera, &'static GlobalTransform),
        With<crate::components::FreeCamera>,
    >,
    state: Res<'w, GizmoState>,
    /// Inserted by `bevy_gizmos_render`'s plugin and by nothing else, so its
    /// presence is the renderer's.
    render: Option<Res<'w, bevy::gizmos_render::LineGizmoEntities>>,
}

impl GizmoReadout<'_, '_> {
    /// Whether gizmo lines can reach the screen at all. `bevy_gizmos` without
    /// `bevy_gizmos_render` accepts every `Gizmos` call and draws nothing.
    pub fn renders(&self) -> bool {
        self.render.is_some()
    }

    pub fn selected(&self) -> usize {
        self.selected.iter().count()
    }

    /// Handles (summed over the selection) on screen whose drawn point, clicked,
    /// selects that same handle. Three per selected prim in view is healthy.
    pub fn axes_hit(&self) -> usize {
        let Ok((camera, cam_gt)) = self.camera.single() else {
            return 0;
        };
        let mode = self.state.mode;
        let mut hit = 0;
        for gt in &self.selected {
            let (pos, arm) = handle_frame(gt);
            for axis in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
                let Ok(screen) =
                    camera.world_to_viewport(cam_gt, drawn_point(pos, arm, axis, mode))
                else {
                    continue;
                };
                if find_hit_axis(camera, cam_gt, pos, arm, screen, mode) == Some(axis) {
                    hit += 1;
                }
            }
        }
        hit
    }
}

/// A point on `axis`'s handle as [`render_gizmo_visuals`] draws it: the tip of an
/// arrow or a scale stub, or an eighth of the way round a ring — not the start,
/// where two rings cross and either answer is right.
fn drawn_point(pos: Vec3, arm: f32, axis: GizmoAxis, mode: GizmoMode) -> Vec3 {
    match mode {
        GizmoMode::Translate | GizmoMode::Scale => pos + axis_vec(axis) * arm,
        GizmoMode::Rotate => {
            let (u, v) = match axis {
                GizmoAxis::X => (Vec3::Y, Vec3::Z),
                GizmoAxis::Y => (Vec3::X, Vec3::Z),
                GizmoAxis::Z => (Vec3::X, Vec3::Y),
            };
            let a = std::f32::consts::FRAC_PI_4;
            pos + (u * a.cos() + v * a.sin()) * arm
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every handle tip must stand clear of the prim it belongs to, or the
    /// arrow is hidden inside it and the point a click needs is behind a face.
    #[test]
    fn handle_tips_clear_the_prim_at_any_rotation() {
        for scale in [
            Vec3::ONE,
            Vec3::splat(0.1),
            Vec3::new(4.0, 0.5, 0.5),
            Vec3::new(20.0, 3.0, 12.0),
        ] {
            let bounding_radius = scale.length() * 0.5;
            let arm = arm_length(scale);
            assert!(
                arm >= bounding_radius + MIN_ARM_CLEARANCE_M - 1e-4,
                "scale {scale}: arm {arm} m leaves under {MIN_ARM_CLEARANCE_M} m past a {bounding_radius} m bounding sphere"
            );
        }
    }

    #[test]
    fn a_mirrored_prim_gets_the_same_handles() {
        assert_eq!(
            arm_length(Vec3::new(-2.0, 1.0, 3.0)),
            arm_length(Vec3::new(2.0, 1.0, 3.0))
        );
    }
}
