//! Prim selection via raycasting (Phase 4 - ADR-017).
//!
//! Left-click: single select (replaces selection).
//! Shift+left-click: toggle individual prim selection.
//! Left-drag on empty space: rubber-band (marquee) multi-select.
//! Shift+drag: add marquee hits to existing selection.
//! Delete / Backspace: delete all selected prims.

use crate::components::PrimMesh;
use crate::components::{Prim, Selected};
use crate::resources::{ContextMenuState, GameState, MarqueeState};
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;
use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::GridCell;

const MARQUEE_DRAG_THRESHOLD_PX: f32 = 4.0;

/// The camera cell, the map anchor and the session, bundled.
///
/// Not a grouping for its own sake: [`prim_picking`] was at Bevy's 16-parameter
/// cap, so anything it learns has to be paid for by something it already knows.
#[derive(bevy::ecs::system::SystemParam)]
pub struct PrimPickWorld<'w, 's> {
    /// The camera's floating-origin cell — the main `cameras` query has no
    /// `GridCell`.
    cameras:
        Query<'w, 's, (&'static GridCell, &'static Transform), With<crate::components::FreeCamera>>,
    anchor: Res<'w, crate::resources::MapAnchor>,
    online: Option<Res<'w, crate::resources::OnlineSession>>,
    /// Whether this frame's press is a finger's — see `EguiManager::owns_click`.
    touch: Res<'w, crate::systems::touch::TouchGestures>,
}

/// Whether the last left click this system processed landed on a prim.
///
/// The world behind the prims wants clicks too — `building_select::pick_building`
/// is the second consumer — and "did the prim layer already take this one?" has to
/// be answerable *after* [`prim_picking`] has run, for a press that selected a
/// prim and for one that merely *touched* a scripted one, which deliberately
/// leaves the selection alone.
///
/// The world behind the prims wants clicks too — `building_select::pick_building`
/// is the second consumer — and "did the prim layer already take this one?" has to
/// be answerable *after* [`prim_picking`] has run, for a press that selected a
/// prim and for one that merely *touched* a scripted one, which deliberately
/// leaves the selection alone.
///
/// Written on **every** press [`prim_picking`] evaluates, hit or miss, which is
/// what keeps it from going stale: the presses it skips are the ones
/// `pick_building` skips too, for the same reasons (egui has the pointer, there is
/// no cursor, there is no camera).
#[derive(Resource, Default)]
pub struct PrimClick {
    took: bool,
}

impl PrimClick {
    /// Did a prim take the most recent press?
    pub fn took(&self) -> bool {
        self.took
    }

    /// Spend this press on something that is not a prim — an avatar
    /// (`contacts::tap_avatar`) — so the building behind it does not take it too.
    pub fn claim(&mut self) {
        self.took = true;
    }
}

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
    selected_query: Query<Entity, With<Selected>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    gizmo_state: Res<crate::systems::gizmo::GizmoState>,
    // For the context menu's geographic point and the touch message. Bundled into
    // one `SystemParam` because Bevy caps a system at 16 parameters and this one
    // was at the cap — the same wall `free_camera::camera_controls` hit when touch
    // input arrived. Three related things that are read together, so the bundle is
    // also the honest grouping.
    world: PrimPickWorld,
    // Whether this press was spent on a prim, for the world behind them.
    mut prim_click: ResMut<PrimClick>,
) {
    let (geo_cameras, map_anchor, online) = (&world.cameras, &world.anchor, &world.online);
    // Don't process world clicks when egui has the pointer (toolbar buttons, dialogs, etc.).
    // The same gate `building_select::pick_building` uses, or `PrimClick` goes stale.
    let cursor = windows.iter().next().and_then(|w| w.cursor_position());
    if egui_manager.owns_click(world.touch.touch_click(), cursor) {
        return;
    }

    let left_pressed = mouse_buttons.just_pressed(MouseButton::Left);
    let right_pressed = mouse_buttons.just_pressed(MouseButton::Right);

    // Any click outside egui closes the context menu.
    if context_menu.visible && (left_pressed || right_pressed) {
        context_menu.visible = false;
    }

    // Picking only reacts to a fresh left/right press. The hit test below is a
    // brute-force ray-vs-box over every prim (with a matrix inverse each); skip it
    // entirely on the (vast majority of) frames with no click to test.
    if !left_pressed && !right_pressed {
        return;
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

    // Recorded before the branches below, and for every press, because what
    // `building_select` needs to know is whether the *ray* found a prim — not what
    // this system then chose to do about it. A scripted prim is worked rather than
    // selected, and a press that works one is still a press that was spent.
    if mouse_buttons.just_pressed(MouseButton::Left) {
        prim_click.took = hit_prim.is_some();
    }

    // Left-click: select / deselect / start marquee
    if mouse_buttons.just_pressed(MouseButton::Left) {
        if gizmo_state.active_axis.is_some() {
            // A gizmo handle drag started on this click (it runs first) — keep the
            // current selection instead of treating the arm click as a world click.
        } else if let Some((entity, prim, _)) = hit_prim {
            // A scripted prim is a *control*, so a plain click works it rather
            // than selecting it — Second Life's rule, and the one everybody
            // expects: you click the button, you do not select the button.
            //
            // Before this, pressing something needed the right-click menu's Touch
            // entry or walking within arm's reach and pressing E, and the beacon
            // on the Grote Markt is what proved that is not discoverable — the
            // person it was built for went looking and could not work it.
            //
            // Shift-click still selects (it is already the selection modifier),
            // and the right-click menu still has Edit and Script, so a builder
            // loses nothing. The selection is deliberately left alone here: a
            // click that both fired a handler and moved the edit target would be
            // two actions on one press.
            if prim.scripted && !shift {
                if let Some(session) = online.as_ref() {
                    session.send(
                        "touch prim",
                        cartopolis_core::NetMessage::TouchPrim {
                            request_id: 0,
                            prim_id: prim.id,
                        },
                    );
                    tracing::debug!(prim_id = prim.id, "prim touched");
                }
                // Offline there is no script to run and nothing to say: a scripted
                // prim cannot exist without a server, so this is unreachable
                // rather than a silent failure.
            } else if shift {
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
            context_menu.hit_point = ray_plane_y(ray_origin, ray_dir, 0.0)
                .unwrap_or_else(|| ray_origin + ray_dir * 10.0);
            tracing::debug!(prim_id = prim.id, "context menu for prim");
        } else {
            context_menu.hit_prim_id = None;

            // The ground plane, which is `y = 0` in the anchored frame — the
            // streamed map is what is under the cursor. It used to be the height
            // of the first `Region` quad in the world, which was zero in every
            // deployment that had one and simply absent in every one that did
            // not, so the menu could not open at all offline.
            context_menu.hit_point = ray_plane_y(ray_origin, ray_dir, 0.0)
                .unwrap_or_else(|| ray_origin + ray_dir * 20.0);
            tracing::debug!("context menu for the map");
        }

        // The geographic point under the cursor, for the routing entries. Built
        // from the ray's *direction* plus the camera's absolute world position —
        // the ray's own origin is render-space, which under `big_space` is the
        // same point `cell` + `tf` name absolutely, so adding both would count the
        // in-cell offset twice (`navigation::handle_route_map_pick` does the same
        // reconstruction and documents it).
        context_menu.lat_lng = map_geo_at(&geo_cameras, ray_dir, &map_anchor);
    }
}

/// `(lat, lng)` where the cursor ray meets the ground plane, or `None` when there
/// is no anchor, no camera, or the ray points at the sky.
fn map_geo_at(
    cameras: &Query<(&GridCell, &Transform), With<crate::components::FreeCamera>>,
    ray_dir: Vec3,
    map_anchor: &crate::resources::MapAnchor,
) -> Option<(f64, f64)> {
    let anchor = map_anchor.geo?;
    let (cell, tf) = cameras.single().ok()?;
    let dir = ray_dir.as_dvec3();
    if dir.y > -1e-4 {
        return None; // at or above the horizon — no ground hit
    }
    let edge = f64::from(crate::grid::WORLD_CELL_EDGE);
    let cam_world = DVec3::new(
        cell.x as f64 * edge + f64::from(tf.translation.x),
        cell.y as f64 * edge + f64::from(tf.translation.y),
        cell.z as f64 * edge + f64::from(tf.translation.z),
    );
    let hit = cam_world + dir * (cam_world.y / -dir.y);
    Some(crate::systems::geo_nav::world_to_lat_lng(
        anchor, hit.x, hit.z,
    ))
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
    if egui_manager.ctx.egui_wants_pointer_input() {
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

    // The drag was measured against the window's cursor, so both corners are in
    // window points and have to be converted before egui paints with them.
    let min = egui.window_to_ui(marquee.start.min(marquee.end));
    let max = egui.window_to_ui(marquee.start.max(marquee.end));
    let rect = egui::Rect::from_min_max(min, max);

    let ctx = egui.ctx_mut();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("marquee_rect"),
    ));

    let fill = egui::Color32::from_rgba_unmultiplied(
        theme::ACCENT.r(),
        theme::ACCENT.g(),
        theme::ACCENT.b(),
        28,
    );
    let stroke = egui::Stroke::new(1.0_f32, theme::ACCENT_BRIGHT);
    painter.rect(
        rect,
        egui::CornerRadius::default(),
        fill,
        stroke,
        egui::StrokeKind::Outside,
    );
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
    // `ui_owns_keyboard`, not `egui_wants_keyboard_input()`. The latter is
    // `memory.focused().is_some()`, which stays true after clicking *any* widget —
    // a button, a selectable — so Delete silently did nothing until you clicked
    // empty space first. CLAUDE.md names this exact predicate as the wrong one and
    // records that it already swallowed the avatar's movement keys once.
    if egui_manager.ui_owns_keyboard(false, &keyboard) {
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
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    selected_query: Query<
        (
            Entity,
            &Prim,
            &MeshMaterial3d<StandardMaterial>,
            Has<crate::components::OwnedMaterial>,
        ),
        (With<Selected>, Changed<Selected>),
    >,
) {
    for (entity, prim, material_handle, owned) in selected_query.iter() {
        // Selecting restyles just this prim, so copy it out of the shared pool first.
        if let Some(mut material) = crate::systems::rendering::owned_material(
            &mut commands,
            &mut materials,
            entity,
            &material_handle.0,
            owned,
        ) {
            // Keep the real surface (color/texture/alpha/glow/uv) and add only a subtle
            // highlight, so the texture stays visible while the prim is selected.
            crate::systems::rendering::apply_surface(&mut material, prim.color, &prim.surface);
            crate::systems::rendering::apply_selection_highlight(&mut material);
        }
    }
}

/// System to restore original color when prim is deselected
pub fn unhighlight_deselected_prim(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    // No With<Selected> filter: entity has already lost the component by the time
    // RemovedComponents fires.
    query: Query<(
        &Prim,
        &MeshMaterial3d<StandardMaterial>,
        Has<crate::components::OwnedMaterial>,
    )>,
    mut deselected: RemovedComponents<Selected>,
) {
    for entity in deselected.read() {
        if let Ok((prim, material_handle, owned)) = query.get(entity) {
            if let Some(mut material) = crate::systems::rendering::owned_material(
                &mut commands,
                &mut materials,
                entity,
                &material_handle.0,
                owned,
            ) {
                // Restore the full surface (alpha/glow/full-bright/uv), not just RGB.
                crate::systems::rendering::apply_surface(&mut material, prim.color, &prim.surface);
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
