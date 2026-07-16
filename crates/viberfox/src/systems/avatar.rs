use bevy::animation::graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeIndex};
use bevy::ecs::query::Or;
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use bevy::world_serialization::{WorldAssetRoot, WorldInstanceReady};
use crate::components::{Avatar, RemoteAvatar, RemoteAvatarMotionHint};
use crate::resources::AvatarState;
use vibe_core::wrap_angle_pi;

// Official Bevy fox model (models/animated/Fox.glb)
const FOX_GLB: &str = "models/animated/Fox.glb";
// Bevy fox animations: 0=Survey (idle), 1=Walk, 2=Run
const IDLE_ANIMATION_INDEX: usize = 0;
const RUN_ANIMATION_INDEX: usize = 2;

/// Online: blend visual toward authoritative sim position (see `smooth_online_avatar_display`).
const ONLINE_DISPLAY_SMOOTHING: f32 = 14.0;
/// Remote rotation should feel a bit snappier than position.
const REMOTE_ROT_SMOOTHING: f32 = 18.0;
/// Horizontal visual speed (m/s) above this plays run animation on remote foxes.
const REMOTE_RUN_SPEED_THRESH: f32 = 0.12;

const WALK_SPEED: f32 = 8.0;
const FLY_SPEED: f32 = 40.0;

/// Horizontal forward from orbit [`CameraState::azimuth`] (into the screen / away from camera).
#[inline]
pub(crate) fn camera_plane_forward(azimuth: f32) -> Vec3 {
    Vec3::new(-azimuth.sin(), 0., -azimuth.cos())
}

#[inline]
pub(crate) fn camera_plane_right(azimuth: f32) -> Vec3 {
    Vec3::new(azimuth.cos(), 0., -azimuth.sin())
}

/// Normalized camera-relative WASD on XZ, or zero.
pub(crate) fn wish_dir_camera_relative(
    azimuth: f32,
    move_forward: bool,
    move_backward: bool,
    move_left: bool,
    move_right: bool,
) -> Vec3 {
    let mut w = Vec3::ZERO;
    let f = camera_plane_forward(azimuth);
    let r = camera_plane_right(azimuth);
    if move_forward {
        w += f;
    }
    if move_backward {
        w -= f;
    }
    if move_right {
        w += r;
    }
    if move_left {
        w -= r;
    }
    let l2 = w.length_squared();
    if l2 > 1e-8 {
        w / l2.sqrt()
    } else {
        Vec3::ZERO
    }
}

/// World yaw for the fox to face camera-forward on the ground (W walks “into” the view; S walks back without turning).
#[inline]
pub(crate) fn fox_facing_yaw_from_camera(azimuth: f32) -> f32 {
    let f = camera_plane_forward(azimuth);
    wrap_angle_pi(f32::atan2(f.x, f.z))
}
const GRAVITY: f32 = -9.8;
const AVATAR_HEIGHT: f32 = 0.8; // Fox model height (scaled)
const GROUND_HEIGHT: f32 = 0.05; // Region tile top surface (cuboid half-extent y)

#[derive(Component)]
pub struct AvatarFoxLoaded;

/// Component storing animation data for the fox, used when scene is ready
#[derive(Component)]
pub(crate) struct FoxAnimationToPlay {
    graph_handle: Handle<AnimationGraph>,
    idle_index: AnimationNodeIndex,
    run_index: AnimationNodeIndex,
}

pub fn spawn_avatar(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    avatar_query: Query<Entity, (Without<AvatarFoxLoaded>, Or<(With<Avatar>, With<RemoteAvatar>)>)>,
) {
    for entity in avatar_query.iter() {
        let (graph, indices) = AnimationGraph::from_clips([
            asset_server.load(GltfAssetLabel::Animation(IDLE_ANIMATION_INDEX).from_asset(FOX_GLB)),
            asset_server.load(GltfAssetLabel::Animation(RUN_ANIMATION_INDEX).from_asset(FOX_GLB)),
        ]);
        let graph_handle = graphs.add(graph);

        let mesh_scene = WorldAssetRoot(
            asset_server.load(GltfAssetLabel::Scene(0).from_asset(FOX_GLB)),
        );

        commands.entity(entity).insert((
            AvatarFoxLoaded,
            FoxAnimationToPlay {
                graph_handle,
                idle_index: indices[0],
                run_index: indices[1],
            },
            mesh_scene,
        )).observe(play_fox_animation_when_ready);
    }
}

fn play_fox_animation_when_ready(
    trigger: On<WorldInstanceReady>,
    mut commands: Commands,
    children: Query<&Children>,
    animations_to_play: Query<&FoxAnimationToPlay>,
    mut players: Query<&mut AnimationPlayer>,
) {
    if let Ok(animation_to_play) = animations_to_play.get(trigger.event_target()) {
        for child in children.iter_descendants(trigger.event_target()) {
            if let Ok(mut player) = players.get_mut(child) {
                // Start with idle; update_fox_animation will switch based on movement
                player.play(animation_to_play.idle_index).repeat().set_speed(1.5);
                commands
                    .entity(child)
                    .insert(AnimationGraphHandle(animation_to_play.graph_handle.clone()));
            }
        }
    }
}

/// Switch fox animation between idle and run based on movement
pub fn update_fox_animation(
    avatar_state: Res<AvatarState>,
    children: Query<&Children>,
    animations_to_play: Query<&FoxAnimationToPlay, With<Avatar>>,
    mut players: Query<&mut AnimationPlayer>,
    avatar_query: Query<Entity, With<Avatar>>,
) {
    for avatar_entity in avatar_query.iter() {
        let Ok(animation_to_play) = animations_to_play.get(avatar_entity) else {
            continue;
        };
        for child in children.iter_descendants(avatar_entity) {
            if let Ok(mut player) = players.get_mut(child) {
                if avatar_state.is_walking {
                    player.stop(animation_to_play.idle_index);
                    if !player.is_playing_animation(animation_to_play.run_index) {
                        player.play(animation_to_play.run_index).repeat().set_speed(1.5);
                    }
                } else {
                    player.stop(animation_to_play.run_index);
                    if !player.is_playing_animation(animation_to_play.idle_index) {
                        player.play(animation_to_play.idle_index).repeat().set_speed(1.5);
                    }
                }
            }
        }
    }
}

/// Interpolate remote entities toward `RemoteAvatar::net_*` so motion matches the local smoothed feel.
pub fn smooth_remote_avatars(
    online: Option<Res<crate::resources::OnlineSession>>,
    time: Res<Time>,
    mut q: Query<(&RemoteAvatar, &mut Transform), (With<RemoteAvatar>, Without<Avatar>)>,
) {
    if online.is_none() {
        return;
    }
    let dt = time.delta_secs();
    let a_pos = 1.0 - (-ONLINE_DISPLAY_SMOOTHING * dt).exp();
    let a_rot = 1.0 - (-REMOTE_ROT_SMOOTHING * dt).exp();
    for (remote, mut tf) in q.iter_mut() {
        tf.translation = tf.translation.lerp(remote.net_position, a_pos);
        // Replicated world yaw from each client’s camera-facing convention.
        let target_rot = Quat::from_rotation_y(wrap_angle_pi(remote.net_yaw));
        tf.rotation = tf.rotation.slerp(target_rot, a_rot);
    }
}

/// Derive horizontal speed from smoothed remote motion for animation blending.
pub fn tick_remote_avatar_motion_hint(
    online: Option<Res<crate::resources::OnlineSession>>,
    time: Res<Time>,
    mut q: Query<(&Transform, &mut RemoteAvatarMotionHint), With<RemoteAvatar>>,
) {
    if online.is_none() {
        return;
    }
    let dt = time.delta_secs().max(1e-4);
    for (tf, mut hint) in q.iter_mut() {
        if !hint.initialized {
            hint.last_translation = tf.translation;
            hint.initialized = true;
            hint.horizontal_speed = 0.0;
            continue;
        }
        let dx = tf.translation.x - hint.last_translation.x;
        let dz = tf.translation.z - hint.last_translation.z;
        hint.horizontal_speed = (dx * dx + dz * dz).sqrt() / dt;
        hint.last_translation = tf.translation;
    }
}

/// Run / idle for other players’ foxes based on [`RemoteAvatarMotionHint::horizontal_speed`].
pub fn update_remote_fox_animation(
    online: Option<Res<crate::resources::OnlineSession>>,
    children: Query<&Children>,
    animations_to_play: Query<&FoxAnimationToPlay, With<RemoteAvatar>>,
    hints: Query<&RemoteAvatarMotionHint, With<RemoteAvatar>>,
    mut players: Query<&mut AnimationPlayer>,
    remote_roots: Query<Entity, With<RemoteAvatar>>,
) {
    if online.is_none() {
        return;
    }
    for avatar_entity in remote_roots.iter() {
        let Ok(animation_to_play) = animations_to_play.get(avatar_entity) else {
            continue;
        };
        let is_walking = hints
            .get(avatar_entity)
            .is_ok_and(|h| h.horizontal_speed > REMOTE_RUN_SPEED_THRESH);
        for child in children.iter_descendants(avatar_entity) {
            if let Ok(mut player) = players.get_mut(child) {
                if is_walking {
                    player.stop(animation_to_play.idle_index);
                    if !player.is_playing_animation(animation_to_play.run_index) {
                        player.play(animation_to_play.run_index).repeat().set_speed(1.5);
                    }
                } else {
                    player.stop(animation_to_play.run_index);
                    if !player.is_playing_animation(animation_to_play.idle_index) {
                        player.play(animation_to_play.idle_index).repeat().set_speed(1.5);
                    }
                }
            }
        }
    }
}

/// When online, the server updates `AvatarState::position` at tick rate; smooth `display_position`
/// and the avatar transform so the third-person camera and world do not jitter.
pub fn smooth_online_avatar_display(
    online: Option<Res<crate::resources::OnlineSession>>,
    mut avatar_state: ResMut<AvatarState>,
    time: Res<Time>,
    mut avatar_query: Query<&mut Transform, With<Avatar>>,
) {
    let dt = time.delta_secs();
    if online.is_some() {
        let alpha = 1.0 - (-ONLINE_DISPLAY_SMOOTHING * dt).exp();
        avatar_state.display_position = avatar_state.display_position.lerp(avatar_state.position, alpha);
        if let Ok(mut tf) = avatar_query.single_mut() {
            tf.translation = avatar_state.display_position;
        }
    } else {
        avatar_state.display_position = avatar_state.position;
    }
}

pub fn handle_avatar_movement(
    online: Option<Res<crate::resources::OnlineSession>>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut avatar_query: Query<&mut Transform, With<Avatar>>,
    mut avatar_state: ResMut<AvatarState>,
    camera_state: Res<crate::resources::CameraState>,
    egui_manager: Res<crate::systems::egui_manager::EguiManager>,
    edit_dialog: Res<crate::resources::EditDialogState>,
) {
    // Don't move avatar unless the follow camera is active (Free and Earth
    // modes decouple the camera from the avatar).
    if camera_state.mode != crate::resources::CameraMode::Avatar {
        avatar_state.is_walking = false;
        return;
    }
    // Don't move while the UI owns the keyboard (text field focused, edit dialog open,
    // or a Ctrl chord like Ctrl+Shift+A); otherwise WASD/S leak into avatar movement.
    if egui_manager.ui_owns_keyboard(edit_dialog.visible, &keyboard_input) {
        avatar_state.is_walking = false;
        return;
    }
    if avatar_query.is_empty() {
        return;
    }

    let Ok(mut transform) = avatar_query.single_mut() else {
        return;
    };
    let delta_time = time.delta().as_secs_f32();

    // Online: server owns translation; camera-relative WASD + fox faces into the orbit camera.
    if online.is_some() {
        if keyboard_input.just_pressed(KeyCode::KeyF) {
            avatar_state.is_flying = !avatar_state.is_flying;
        }
        let move_left =
            keyboard_input.pressed(KeyCode::KeyA) || keyboard_input.pressed(KeyCode::ArrowLeft);
        let move_right =
            keyboard_input.pressed(KeyCode::KeyD) || keyboard_input.pressed(KeyCode::ArrowRight);
        let move_forward =
            keyboard_input.pressed(KeyCode::KeyW) || keyboard_input.pressed(KeyCode::ArrowUp);
        let move_backward =
            keyboard_input.pressed(KeyCode::KeyS) || keyboard_input.pressed(KeyCode::ArrowDown);
        avatar_state.is_walking =
            move_forward || move_backward || move_left || move_right;
        let az = camera_state.azimuth;
        let face = fox_facing_yaw_from_camera(az);
        let pi = std::f32::consts::PI;
        avatar_state.online_tank_yaw = wrap_angle_pi(face - pi);
        transform.rotation = Quat::from_rotation_y(face);
        return;
    }

    // Always sync avatar state position with transform
    // This ensures camera can follow the avatar correctly
    avatar_state.position = transform.translation;
    avatar_state.display_position = transform.translation;

    // Toggle fly mode with F key
    if keyboard_input.just_pressed(KeyCode::KeyF) {
        avatar_state.is_flying = !avatar_state.is_flying;
        println!("Fly mode: {}", if avatar_state.is_flying { "ON" } else { "OFF" });
    }

    let move_forward = keyboard_input.pressed(KeyCode::KeyW) || keyboard_input.pressed(KeyCode::ArrowUp);
    let move_backward = keyboard_input.pressed(KeyCode::KeyS) || keyboard_input.pressed(KeyCode::ArrowDown);
    let move_left = keyboard_input.pressed(KeyCode::KeyA) || keyboard_input.pressed(KeyCode::ArrowLeft);
    let move_right = keyboard_input.pressed(KeyCode::KeyD) || keyboard_input.pressed(KeyCode::ArrowRight);
    let fly_up = keyboard_input.pressed(KeyCode::Space);
    let fly_down = keyboard_input.pressed(KeyCode::ShiftLeft) || keyboard_input.pressed(KeyCode::ShiftRight);

    let az = camera_state.azimuth;
    let wish = wish_dir_camera_relative(az, move_forward, move_backward, move_left, move_right);
    if wish.length_squared() > 1e-8 {
        let speed = if avatar_state.is_flying { FLY_SPEED } else { WALK_SPEED };
        let delta = wish * speed * delta_time;
        transform.translation.x += delta.x;
        transform.translation.z += delta.z;
        avatar_state.is_walking = true;
    } else {
        avatar_state.is_walking = false;
    }

    // Handle vertical movement
    if avatar_state.is_flying {
        if fly_up {
            transform.translation.y += FLY_SPEED * delta_time;
        } else if fly_down {
            let min_height = GROUND_HEIGHT + AVATAR_HEIGHT / 2.0;
            if transform.translation.y > min_height + 0.1 {
                transform.translation.y -= FLY_SPEED * delta_time;
            }
        }
    } else {
        // Walking mode: apply gravity
        transform.translation.y += GRAVITY * delta_time;

        // Enforce minimum height
        let min_height = GROUND_HEIGHT + AVATAR_HEIGHT / 2.0;
        if transform.translation.y < min_height {
            transform.translation.y = min_height;
        }
    }

    let face = fox_facing_yaw_from_camera(az);
    let pi = std::f32::consts::PI;
    avatar_state.rotation = wrap_angle_pi(face - pi);
    transform.rotation = Quat::from_rotation_y(face);

    // Update avatar state position to match transform (important for camera following)
    avatar_state.position = transform.translation;
    avatar_state.display_position = transform.translation;
}
