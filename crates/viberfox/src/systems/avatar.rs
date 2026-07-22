use bevy::animation::graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeIndex};
use bevy::animation::{AnimatedBy, AnimationTargetId};
use bevy::ecs::query::Or;
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use bevy::world_serialization::{WorldAssetRoot, WorldInstanceReady};
use crate::components::{Avatar, RemoteAvatar, RemoteAvatarMotionHint};
use crate::resources::AvatarState;
use crate::systems::user_store::{AvatarAppearance, AvatarSex, ClothingTheme, UserStore};
use vibe_core::wrap_angle_pi;

/// A concrete, loadable avatar as one or two glTF scenes on the same rig, driven
/// by clips from `anims`. Which scenes are present depends on how a Quaternius
/// outfit is authored (see `resolve_avatar_model`):
/// - `body` only — a bare base body (Casual), or the fox.
/// - `body` + `outfit` — the outfit has no head mesh (Peasant), so the base body
///   supplies the head. Caveat: these outfits are partial-coverage (bare-torso +
///   sleeveless), so the base body's bare back still shows — an asset limitation.
/// - `outfit` only — a self-complete outfit (Ranger: bare-chested + hood). A base
///   body here would double the torso and poke through the clothing.
pub(crate) struct AvatarModel {
    body: Option<&'static str>,
    outfit: Option<&'static str>,
    anims: &'static str,
    scale: f32,
    idle_ix: usize,
    run_ix: usize,
    fly_ix: usize,
    anim_speed: f32,
}

/// Quaternius Universal Animation Library (CC0). Clips bind to any body sharing
/// the universal `Armature → root → pelvis` rig (the base bodies and outfits do).
const UAL: &str = "models/avatar/anims/UAL1_Standard.glb";
// Clip indices within UAL1_Standard.glb (see `jq '.animations'`).
const UAL_IDLE: usize = 9; // Idle_Loop
const UAL_JOG: usize = 13; // Jog_Fwd_Loop (used as the "moving" gait)
const UAL_FLY: usize = 16; // Jump_Loop — a mid-air pose, reused as the flying stance

/// The Bevy sample fox — the fallback if the humanoid glTFs are missing. Its
/// clips live in the same file (0=Survey/idle, 2=Run); it has no fly pose, so
/// idle stands in.
const FOX_MODEL: AvatarModel = AvatarModel {
    body: Some("models/animated/Fox.glb"),
    outfit: None,
    anims: "models/animated/Fox.glb",
    scale: 0.02,
    idle_ix: 0,
    run_ix: 2,
    fly_ix: 0,
    anim_speed: 1.5,
};

/// Build a humanoid model from an optional base body + optional outfit (at least
/// one must be present). Bodies are authored in metres (~1.8 m tall), so world
/// scale is 1.0; clips come from the UAL.
const fn humanoid(body: Option<&'static str>, outfit: Option<&'static str>) -> AvatarModel {
    AvatarModel {
        body,
        outfit,
        anims: UAL,
        scale: 1.0,
        idle_ix: UAL_IDLE,
        run_ix: UAL_JOG,
        fly_ix: UAL_FLY,
        anim_speed: 1.0,
    }
}

/// Resolve the model for an appearance. Which scenes are used depends on how the
/// outfit is authored (the Quaternius packs are not uniform):
/// - Casual → base body only.
/// - Peasant → base body + outfit: the outfit has no head mesh, so the base body
///   provides the head. The peasant clothing is only partial (sleeveless, open
///   back), so the base's bare back still shows — a limitation of that asset.
/// - Ranger → outfit only: it's a self-complete, bare-chested + hooded character;
///   a base body underneath would double the torso and show through the skin.
/// Falls back to the fox if the needed glTFs are missing.
pub(crate) fn resolve_avatar_model(appearance: AvatarAppearance) -> AvatarModel {
    use AvatarSex::*;
    use ClothingTheme::*;
    let base = match appearance.sex {
        Man => "models/avatar/base/Superhero_Male_FullBody.gltf",
        Woman => "models/avatar/base/Superhero_Female_FullBody.gltf",
    };
    let (body, outfit): (Option<&str>, Option<&str>) = match (appearance.sex, appearance.theme) {
        (_, Casual) => (Some(base), None),
        (Man, Peasant) => (Some(base), Some("models/avatar/outfits/Male_Peasant.gltf")),
        (Woman, Peasant) => (Some(base), Some("models/avatar/outfits/Female_Peasant.gltf")),
        (Man, Ranger) => (None, Some("models/avatar/outfits/Male_Ranger.gltf")),
        (Woman, Ranger) => (None, Some("models/avatar/outfits/Female_Ranger.gltf")),
    };
    // Keep only scenes that are actually on disk.
    let body = body.filter(|s| avatar_asset_exists(s));
    let outfit = outfit.filter(|s| avatar_asset_exists(s));
    if body.is_none() && outfit.is_none() {
        FOX_MODEL // assets not fetched
    } else {
        humanoid(body, outfit)
    }
}

/// Whether an avatar glTF is present under the runtime `assets/` root. Lets the
/// client fall back to the fox instead of spawning an invisible avatar when the
/// (large, gitignored) Quaternius packs haven't been fetched.
fn avatar_asset_exists(scene_rel: &str) -> bool {
    std::path::Path::new("assets").join(scene_rel).is_file()
}

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

/// World yaw for the avatar to face camera-forward on the ground (W walks “into” the view; S walks back without turning).
#[inline]
pub(crate) fn avatar_facing_yaw_from_camera(azimuth: f32) -> f32 {
    let f = camera_plane_forward(azimuth);
    wrap_angle_pi(f32::atan2(f.x, f.z))
}
const GRAVITY: f32 = -9.8;
const GROUND_HEIGHT: f32 = 0.05; // Region tile top surface (cuboid half-extent y)

#[derive(Component)]
pub struct AvatarModelLoaded;

/// Animation data attached to an avatar (and each of its model "holder" children),
/// read once a scene is ready. `Clone` so the body and outfit holders can share it.
#[derive(Component, Clone)]
pub(crate) struct AvatarAnimations {
    graph_handle: Handle<AnimationGraph>,
    idle_index: AnimationNodeIndex,
    run_index: AnimationNodeIndex,
    fly_index: AnimationNodeIndex,
    /// Playback speed for this model's clips (`AvatarModel::anim_speed`).
    speed: f32,
}

pub fn spawn_avatar(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    store: Res<UserStore>,
    mut avatar_query: Query<
        (Entity, Has<Avatar>, &mut Transform),
        (Without<AvatarModelLoaded>, Or<(With<Avatar>, With<RemoteAvatar>)>),
    >,
) {
    for (entity, is_local, mut transform) in avatar_query.iter_mut() {
        // The local player uses the chosen appearance; remotes use the default
        // until appearance is replicated over the wire (future protocol change).
        let appearance = if is_local {
            store.data.appearance
        } else {
            AvatarAppearance::default()
        };
        let model = resolve_avatar_model(appearance);

        let (graph, indices) = AnimationGraph::from_clips([
            asset_server.load(GltfAssetLabel::Animation(model.idle_ix).from_asset(model.anims)),
            asset_server.load(GltfAssetLabel::Animation(model.run_ix).from_asset(model.anims)),
            asset_server.load(GltfAssetLabel::Animation(model.fly_ix).from_asset(model.anims)),
        ]);
        let graph_handle = graphs.add(graph);
        let anims = AvatarAnimations {
            graph_handle,
            idle_index: indices[0],
            run_index: indices[1],
            fly_index: indices[2],
            speed: model.anim_speed,
        };

        // Scale lives with the model, not the spawn site. Set it before the scenes
        // instantiate (no mesh is attached yet) so they never flash at 1.0.
        transform.scale = Vec3::splat(model.scale);

        // The body and (optional) outfit are separate glTF scenes on the SAME rig.
        // Each goes in its own child "holder" so a per-scene `WorldInstanceReady`
        // observer only walks its own subtree when binding the shared UAL clips —
        // otherwise the body's observer would wander into the outfit's skeleton.
        // Both holders play the same graph, so body and clothing move in lockstep.
        // `AvatarAnimations` is also kept on the avatar entity for the idle/run/fly
        // driver (`update_avatar_animation`) to read the clip indices.
        commands.entity(entity).insert((AvatarModelLoaded, anims.clone()));
        if let Some(body) = model.body {
            spawn_model_holder(&mut commands, &asset_server, entity, body, anims.clone());
        }
        if let Some(outfit) = model.outfit {
            spawn_model_holder(&mut commands, &asset_server, entity, outfit, anims);
        }
    }
}

/// Spawn one glTF scene as a child holder of `parent`, with its own animation
/// data + ready observer. Identity transform, so it inherits the avatar's pose
/// and scale exactly.
fn spawn_model_holder(
    commands: &mut Commands,
    asset_server: &AssetServer,
    parent: Entity,
    scene: &'static str,
    anims: AvatarAnimations,
) {
    commands
        .spawn((
            anims,
            WorldAssetRoot(asset_server.load(GltfAssetLabel::Scene(0).from_asset(scene))),
            Transform::default(),
            Visibility::default(),
            ChildOf(parent),
        ))
        .observe(play_avatar_animation_when_ready);
}

/// Rebuild the local avatar's model when the chosen appearance changes (from the
/// avatar chooser). Strips the instantiated scene and the loaded markers so
/// `spawn_avatar` re-runs next frame with the newly resolved model. Remotes are
/// untouched — they don't read the local appearance.
pub fn respawn_avatar_on_appearance_change(
    mut commands: Commands,
    store: Res<UserStore>,
    mut last: Local<Option<AvatarAppearance>>,
    children: Query<&Children>,
    avatars: Query<Entity, (With<Avatar>, With<AvatarModelLoaded>)>,
) {
    let current = store.data.appearance;
    match *last {
        None => {
            *last = Some(current);
            return;
        }
        Some(prev) if prev == current => return,
        _ => {}
    }
    *last = Some(current);
    for entity in avatars.iter() {
        // Despawn the instantiated scene (all descendants), then drop the markers
        // and the scene root so `spawn_avatar` treats the entity as fresh.
        let descendants: Vec<Entity> = children.iter_descendants(entity).collect();
        for d in descendants {
            commands.entity(d).despawn();
        }
        commands
            .entity(entity)
            .remove::<AvatarModelLoaded>()
            .remove::<AvatarAnimations>()
            .remove::<WorldAssetRoot>();
    }
}

/// glTF node name of the shared skeleton root in every Quaternius rig (base
/// bodies, outfits, and the UAL animation library). It is the scene's top-level
/// node, so the loader roots animation-target paths at it — see
/// [`bind_external_animations`].
const ARMATURE_NODE: &str = "Armature";

pub fn play_avatar_animation_when_ready(
    trigger: On<WorldInstanceReady>,
    mut commands: Commands,
    children: Query<&Children>,
    names: Query<&Name>,
    animations_to_play: Query<&AvatarAnimations>,
    mut players: Query<&mut AnimationPlayer>,
) {
    let root = trigger.event_target();
    let Ok(anim) = animations_to_play.get(root) else {
        return;
    };

    // Case 1 — the model's own glTF embeds its clips (the fox), so Bevy already
    // spawned an `AnimationPlayer`. Play the idle clip on it directly.
    if let Some(player_entity) = children
        .iter_descendants(root)
        .find(|&e| players.get(e).is_ok())
    {
        if let Ok(mut player) = players.get_mut(player_entity) {
            player.play(anim.idle_index).repeat().set_speed(anim.speed);
            commands
                .entity(player_entity)
                .insert(AnimationGraphHandle(anim.graph_handle.clone()));
        }
        return;
    }

    // Case 2 — the humanoid bodies carry no clips of their own, so the glTF
    // loader created neither an `AnimationPlayer` nor the per-bone
    // `AnimationTargetId`/`AnimatedBy` that the external UAL clips bind to. Rebuild
    // both by hand, exactly as `bevy_gltf` would for an animated file: root a
    // player on the "Armature" node and hash each bone's name-path into the target
    // id it was authored against. Both the bodies and the UAL share the same
    // `Armature → root → pelvis → …` rig, so the ids line up and the clips play.
    let Some(armature) = children
        .iter_descendants(root)
        .find(|&e| names.get(e).is_ok_and(|n| n.as_str() == ARMATURE_NODE))
    else {
        tracing::warn!(scene_root = ?root, "avatar: no `Armature` node found; animations will not play");
        return;
    };
    let mut path: Vec<Name> = Vec::new();
    bind_external_animations(&mut commands, armature, armature, &mut path, &children, &names);

    let mut player = AnimationPlayer::default();
    player.play(anim.idle_index).repeat().set_speed(anim.speed);
    commands
        .entity(armature)
        .insert((player, AnimationGraphHandle(anim.graph_handle.clone())));
}

/// Tag `entity` and every named descendant with the `AnimationTargetId` its
/// name-path hashes to (rooted at `player`), plus `AnimatedBy(player)`, mirroring
/// `bevy_gltf`'s animation-target setup. This lets clips from a *separate* glTF
/// bind to a skeleton whose own file had no animations. Unnamed entities (skinned
/// mesh leaves) are skipped along with their subtrees — bones are always named.
fn bind_external_animations(
    commands: &mut Commands,
    entity: Entity,
    player: Entity,
    path: &mut Vec<Name>,
    children: &Query<&Children>,
    names: &Query<&Name>,
) {
    let Ok(name) = names.get(entity) else {
        return;
    };
    path.push(name.clone());
    commands
        .entity(entity)
        .insert((AnimationTargetId::from_names(path.iter()), AnimatedBy(player)));
    if let Ok(kids) = children.get(entity) {
        for child in kids.iter() {
            bind_external_animations(commands, child, player, path, children, names);
        }
    }
    path.pop();
}

/// The locomotion clip an avatar should currently loop.
#[derive(Clone, Copy)]
enum Gait {
    Idle,
    Run,
    Fly,
}

/// Drive one `AnimationPlayer` to loop the clip for `gait`, stopping the others.
/// Applied to every player under an avatar (body + outfit holders) so they stay
/// in lockstep.
fn play_gait(player: &mut AnimationPlayer, anim: &AvatarAnimations, gait: Gait) {
    let (want, stop_a, stop_b) = match gait {
        Gait::Idle => (anim.idle_index, anim.run_index, anim.fly_index),
        Gait::Run => (anim.run_index, anim.idle_index, anim.fly_index),
        Gait::Fly => (anim.fly_index, anim.idle_index, anim.run_index),
    };
    player.stop(stop_a);
    player.stop(stop_b);
    if !player.is_playing_animation(want) {
        player.play(want).repeat().set_speed(anim.speed);
    }
}

/// Switch the local avatar's animation between idle, run, and fly based on state.
pub fn update_avatar_animation(
    avatar_state: Res<AvatarState>,
    children: Query<&Children>,
    animations_to_play: Query<&AvatarAnimations, With<Avatar>>,
    mut players: Query<&mut AnimationPlayer>,
    avatar_query: Query<Entity, With<Avatar>>,
) {
    let gait = if avatar_state.is_flying {
        Gait::Fly
    } else if avatar_state.is_walking {
        Gait::Run
    } else {
        Gait::Idle
    };
    for avatar_entity in avatar_query.iter() {
        let Ok(anim) = animations_to_play.get(avatar_entity) else {
            continue;
        };
        for child in children.iter_descendants(avatar_entity) {
            if let Ok(mut player) = players.get_mut(child) {
                play_gait(&mut player, anim, gait);
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

/// Run / idle for other players’ avatars based on [`RemoteAvatarMotionHint::horizontal_speed`].
pub fn update_remote_avatar_animation(
    online: Option<Res<crate::resources::OnlineSession>>,
    children: Query<&Children>,
    animations_to_play: Query<&AvatarAnimations, With<RemoteAvatar>>,
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
        // Remotes only replicate horizontal motion, so idle/run (no fly state).
        let gait = if hints
            .get(avatar_entity)
            .is_ok_and(|h| h.horizontal_speed > REMOTE_RUN_SPEED_THRESH)
        {
            Gait::Run
        } else {
            Gait::Idle
        };
        for child in children.iter_descendants(avatar_entity) {
            if let Ok(mut player) = players.get_mut(child) {
                play_gait(&mut player, animation_to_play, gait);
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
        let face = avatar_facing_yaw_from_camera(az);
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

    // Handle vertical movement. The avatar models are authored origin-at-feet, so
    // the standing height is the ground surface itself (`GROUND_HEIGHT`) — the old
    // `+ AVATAR_HEIGHT/2` suited only the fox, whose origin is its centre, and it
    // floated the humanoids ~0.45 m.
    if avatar_state.is_flying {
        if fly_up {
            transform.translation.y += FLY_SPEED * delta_time;
        } else if fly_down {
            if transform.translation.y > GROUND_HEIGHT + 0.1 {
                transform.translation.y -= FLY_SPEED * delta_time;
            }
        }
    } else {
        // Walking mode: apply gravity, then rest the feet on the ground.
        transform.translation.y += GRAVITY * delta_time;
        if transform.translation.y < GROUND_HEIGHT {
            transform.translation.y = GROUND_HEIGHT;
        }
    }

    let face = avatar_facing_yaw_from_camera(az);
    let pi = std::f32::consts::PI;
    avatar_state.rotation = wrap_angle_pi(face - pi);
    transform.rotation = Quat::from_rotation_y(face);

    // Update avatar state position to match transform (important for camera following)
    avatar_state.position = transform.translation;
    avatar_state.display_position = transform.translation;
}
