use bevy::animation::graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeIndex};
use bevy::animation::{AnimatedBy, AnimationTargetId};
use bevy::ecs::query::Or;
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use bevy::world_serialization::{WorldAssetRoot, WorldInstanceReady};
use crate::components::{Avatar, RemoteAvatar, RemoteAvatarMotionHint};
use crate::resources::AvatarState;
use crate::systems::user_store::{AvatarAppearance, AvatarSex, UserStore};
use vibe_core::wrap_angle_pi;

/// One glTF scene to spawn for an avatar, showing only the meshes for the slots in
/// `mask`. Complete Quaternius characters embed their own clips, so `anims` is the
/// same file. Several holders (different character files, different slot masks)
/// compose one mixed avatar on the shared rig.
struct Holder {
    scene: &'static str,
    anims: &'static str,
    mask: SlotMask,
    scale: f32,
    idle_ix: usize,
    run_ix: usize,
    fly_ix: usize,
    anim_speed: f32,
}

/// A customization slot within a Quaternius character (identified by mesh-node
/// suffix). `Head` includes face/hair/headwear.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Head,
    Top,
    Legs,
    Feet,
    Accessory,
}

/// Which slots a holder contributes; meshes for other slots are hidden on spawn.
#[derive(Component, Clone, Copy, Default)]
pub(crate) struct SlotMask {
    head: bool,
    top: bool,
    legs: bool,
    feet: bool,
    accessory: bool,
}

impl SlotMask {
    const ALL: Self = Self { head: true, top: true, legs: true, feet: true, accessory: true };
    fn shows(&self, slot: Slot) -> bool {
        match slot {
            Slot::Head => self.head,
            Slot::Top => self.top,
            Slot::Legs => self.legs,
            Slot::Feet => self.feet,
            Slot::Accessory => self.accessory,
        }
    }
    fn set(&mut self, slot: Slot) {
        match slot {
            Slot::Head => self.head = true,
            Slot::Top => self.top = true,
            Slot::Legs => self.legs = true,
            Slot::Feet => self.feet = true,
            Slot::Accessory => self.accessory = true,
        }
    }
}

/// Classify a character mesh-node name into its slot by suffix, or `None` for nodes
/// we don't slot — weapons are hidden separately (see `is_weapon_node`), and the
/// fox's unslotted meshes return `None` so they always show.
fn classify_slot(name: &str) -> Option<Slot> {
    if name.ends_with("_Head") {
        Some(Slot::Head)
    } else if name.ends_with("_Body") {
        Some(Slot::Top)
    } else if name.ends_with("_Legs") || name.ends_with("_Pants") {
        Some(Slot::Legs)
    } else if name.ends_with("_Feet") {
        Some(Slot::Feet)
    } else if name.contains("Backpack") {
        Some(Slot::Accessory)
    } else {
        None
    }
}

// Clip indices shared by every Quaternius "Ultimate Modular" character glTF (they
// all embed the same 24-clip set — see `jq '.animations'`).
const CHAR_IDLE: usize = 4; // Idle
const CHAR_RUN: usize = 16; // Run
// The set has no jump/fly clip, so Idle doubles as the airborne pose — what makes
// it read as flight is the forward lean in `update_avatar_flight_pose`.

/// A holder for one Quaternius character glTF (~1.8 m, scale 1.0; self-animated),
/// showing the slots in `mask`.
fn character_holder(scene: &'static str, mask: SlotMask) -> Holder {
    Holder {
        scene,
        anims: scene,
        mask,
        scale: 1.0,
        idle_ix: CHAR_IDLE,
        run_ix: CHAR_RUN,
        fly_ix: CHAR_IDLE,
        anim_speed: 1.0,
    }
}

/// A selectable avatar: a display `label` (also the stored `style` id) for a given
/// `sex`, mapped to the complete self-animated character glTF it loads.
pub(crate) struct CharacterStyle {
    pub sex: AvatarSex,
    pub label: &'static str,
    pub file: &'static str,
}

macro_rules! man {
    ($label:literal, $file:literal) => {
        CharacterStyle { sex: AvatarSex::Man, label: $label, file: concat!("models/avatar/characters/", $file) }
    };
}
macro_rules! woman {
    ($label:literal, $file:literal) => {
        CharacterStyle { sex: AvatarSex::Woman, label: $label, file: concat!("models/avatar/characters/", $file) }
    };
}

/// Catalog of Quaternius "Ultimate Modular" characters (CC0), grouped by sex in
/// chooser-dropdown order. Add a row (and drop the glTF in) to expose a style.
pub(crate) const CHARACTER_STYLES: &[CharacterStyle] = &[
    man!("Casual", "Man_Casual.gltf"),
    man!("Business", "Man_Suit.gltf"),
    man!("Adventurer", "Man_Adventurer.gltf"),
    man!("Worker", "Man_Worker.gltf"),
    man!("Farmer", "Man_Farmer.gltf"),
    man!("Beach", "Man_Beach.gltf"),
    man!("King", "Man_King.gltf"),
    man!("Space Suit", "Man_Spacesuit.gltf"),
    man!("Swat", "Man_Swat.gltf"),
    man!("Punk", "Man_Punk.gltf"),
    woman!("Casual", "Woman_Casual.gltf"),
    woman!("Business", "Woman_Suit.gltf"),
    woman!("Formal", "Woman_Formal.gltf"),
    woman!("Adventurer", "Woman_Adventurer.gltf"),
    woman!("Worker", "Woman_Worker.gltf"),
    woman!("Medieval", "Woman_Medieval.gltf"),
    woman!("Sci-Fi", "Woman_SciFi.gltf"),
    woman!("Soldier", "Woman_Soldier.gltf"),
    woman!("Witch", "Woman_Witch.gltf"),
    woman!("Punk", "Woman_Punk.gltf"),
];

/// Style labels available for a sex, in catalog order (drives the chooser dropdown).
pub(crate) fn styles_for(sex: AvatarSex) -> impl Iterator<Item = &'static str> {
    CHARACTER_STYLES
        .iter()
        .filter(move |c| c.sex == sex)
        .map(|c| c.label)
}

/// Styles that contribute a wearable (non-weapon) accessory mesh — currently only
/// the Adventurer's backpack. The chooser lists these (plus "None") for `sex`.
const ACCESSORY_STYLES: &[(AvatarSex, &str)] = &[(AvatarSex::Man, "Adventurer")];

pub(crate) fn accessory_styles_for(sex: AvatarSex) -> impl Iterator<Item = &'static str> {
    ACCESSORY_STYLES
        .iter()
        .filter(move |(s, _)| *s == sex)
        .map(|(_, label)| *label)
}

/// The character file for a (sex, style) that is present on disk, or `None`.
fn style_file(sex: AvatarSex, style: &str) -> Option<&'static str> {
    CHARACTER_STYLES
        .iter()
        .find(|c| c.sex == sex && c.label == style)
        .map(|c| c.file)
        .filter(|f| avatar_asset_exists(f))
}

/// Add `slot` to the holder group for `file` (creating it), if the file exists.
fn add_slot(groups: &mut Vec<(&'static str, SlotMask)>, file: Option<&'static str>, slot: Slot) {
    let Some(f) = file else { return };
    match groups.iter_mut().find(|(g, _)| *g == f) {
        Some((_, mask)) => mask.set(slot),
        None => {
            let mut mask = SlotMask::default();
            mask.set(slot);
            groups.push((f, mask));
        }
    }
}

/// Resolve an appearance into the holders to spawn: one per distinct character glTF
/// used across the slots, each carrying the slots it contributes. A preset (all
/// slots one style) collapses to a single holder. Empty if the character glTFs
/// aren't present (e.g. a clone without Git LFS) — the avatar is then invisible.
fn resolve_avatar_holders(a: &AvatarAppearance) -> Vec<Holder> {
    let mut groups: Vec<(&'static str, SlotMask)> = Vec::new();
    add_slot(&mut groups, style_file(a.sex, &a.head), Slot::Head);
    add_slot(&mut groups, style_file(a.sex, &a.top), Slot::Top);
    add_slot(&mut groups, style_file(a.sex, &a.legs), Slot::Legs);
    add_slot(&mut groups, style_file(a.sex, &a.feet), Slot::Feet);
    if let Some(acc) = &a.accessory {
        add_slot(&mut groups, style_file(a.sex, acc), Slot::Accessory);
    }
    groups
        .into_iter()
        .map(|(f, mask)| character_holder(f, mask))
        .collect()
}

/// Whether an avatar glTF is present under the runtime `assets/` root. Lets the
/// client skip a missing character (rather than spawning an invisible avatar) when the
/// (large, gitignored) Quaternius packs haven't been fetched.
fn avatar_asset_exists(scene_rel: &str) -> bool {
    std::path::Path::new("assets").join(scene_rel).is_file()
}

/// Online: blend visual toward authoritative sim position (see `smooth_online_avatar_display`).
const ONLINE_DISPLAY_SMOOTHING: f32 = 14.0;
/// Online prediction reconcile rate: how quickly the locally-predicted display
/// position is corrected toward the authoritative sim position. Since prediction
/// integrates the same velocity the sim does, the residual error is small, so
/// this can be brisk without the tick-rate pulsing the old ease-to-latest had.
const ONLINE_RECONCILE_RATE: f32 = 9.0;
/// Cap on how far the authoritative pose is extrapolated while waiting for the
/// next snapshot (see `smooth_online_avatar_display`). Just over one 20 Hz
/// snapshot period: enough to bridge the normal gap, short enough that a stalled
/// connection parks the avatar instead of flying it off on a stale velocity.
const ONLINE_MAX_EXTRAPOLATION: f32 = 0.15;
/// Prediction error (metres) beyond which the display snaps instead of easing —
/// a teleport, a server correction or a resumed connection, not drift.
const ONLINE_SNAP_ERROR: f32 = 8.0;
/// Remote rotation should feel a bit snappier than position.
const REMOTE_ROT_SMOOTHING: f32 = 18.0;
/// Horizontal visual speed (m/s) above this plays run animation on remote foxes.
const REMOTE_RUN_SPEED_THRESH: f32 = 0.12;

const WALK_SPEED: f32 = 8.0;
/// Offline flight, mirroring what the sim gives an online client so the same keys
/// move the avatar at the same rate in every run mode: horizontal is the sim's
/// speed cap (`SimWorld::apply_intent` MAX_SPEED, which `FLY_MOVE_MULT` reaches),
/// vertical and the fall rate are `vibe_sim::state`'s. Offline used to fly 40 m/s
/// in both axes — a third faster sideways, three times faster up.
const FLY_SPEED: f32 = 32.0;
const FLY_VERTICAL_SPEED: f32 = 12.0;
const FALL_SPEED: f32 = 12.0;

/// Turn rate for the avatar's facing: an exponential ease with a hard cap, so a
/// tap on A snaps crisply to the new heading while an about-face (S) sweeps
/// through the intervening angle in ~0.3 s instead of popping.
const TURN_SMOOTHING: f32 = 16.0;
const MAX_TURN_RATE: f32 = 12.0;

/// The pitch a flying avatar leans into its travel direction, pivoting about
/// chest height so the feet swing back rather than the model sinking through the
/// ground. The Quaternius set has no flight clip (see `docs/notes/avatar-locomotion.md`),
/// so the pose is what distinguishes gliding from standing in mid-air.
const FLIGHT_LEAN: f32 = 1.05; // ~60°
const FLIGHT_LEAN_PIVOT_Y: f32 = 0.95;
const FLIGHT_LEAN_SMOOTHING: f32 = 6.0;

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

/// World yaw that turns the model's forward (+Z) onto `dir` on the XZ plane, or
/// `None` for no input — an avatar standing still keeps the heading it has rather
/// than spinning with the camera.
#[inline]
pub(crate) fn yaw_from_dir(dir: Vec3) -> Option<f32> {
    if dir.length_squared() < 1e-8 {
        None
    } else {
        Some(wrap_angle_pi(f32::atan2(dir.x, dir.z)))
    }
}

/// Step the avatar's yaw toward `target` for one frame: the avatar turns to face
/// where it is walking, in every direction. It used to be pinned to camera-forward,
/// which made A/D a sideways moonwalk and S a backwards one.
pub(crate) fn advance_facing(current: f32, target: f32, dt: f32) -> f32 {
    let delta = wrap_angle_pi(target - current);
    // Settle exactly, or the ease creeps forever and keeps rewriting the transform.
    if delta.abs() < 1e-3 {
        return wrap_angle_pi(target);
    }
    let ease = delta * (1.0 - (-TURN_SMOOTHING * dt).exp());
    let cap = MAX_TURN_RATE * dt;
    wrap_angle_pi(current + ease.clamp(-cap, cap))
}

const GROUND_HEIGHT: f32 = 0.05; // Region tile top surface (cuboid half-extent y)
/// Height above the ground at which the avatar stops being a pedestrian: the
/// building footprints are 2D, so they only apply to something standing on them.
pub(crate) const AIRBORNE_EPS: f32 = 0.2;

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
            store.data.appearance.clone()
        } else {
            AvatarAppearance::default()
        };
        let holders = resolve_avatar_holders(&appearance);

        // Scale lives with the model, not the spawn site. Set it before the scenes
        // instantiate (no mesh is attached yet) so they never flash at 1.0.
        transform.scale = Vec3::splat(holders.first().map_or(1.0, |h| h.scale));

        // One child "holder" per distinct character glTF: it shows only its masked
        // slots (the others are hidden on ready) and plays its own embedded clips.
        // All holders play the identical clip set on the identical rig and are
        // phase-locked by `sync_avatar_animations`, so a mixed head/body/legs moves
        // as one. A representative `AvatarAnimations` is kept on the avatar entity so
        // `update_avatar_animation` can read the (shared) clip indices.
        commands.entity(entity).insert(AvatarModelLoaded);
        let mut entity_anims: Option<AvatarAnimations> = None;
        for h in &holders {
            let (graph, indices) = AnimationGraph::from_clips([
                asset_server.load(GltfAssetLabel::Animation(h.idle_ix).from_asset(h.anims)),
                asset_server.load(GltfAssetLabel::Animation(h.run_ix).from_asset(h.anims)),
                asset_server.load(GltfAssetLabel::Animation(h.fly_ix).from_asset(h.anims)),
            ]);
            let anims = AvatarAnimations {
                graph_handle: graphs.add(graph),
                idle_index: indices[0],
                run_index: indices[1],
                fly_index: indices[2],
                speed: h.anim_speed,
            };
            if entity_anims.is_none() {
                entity_anims = Some(anims.clone());
            }
            spawn_slot_holder(&mut commands, &asset_server, entity, h.scene, h.mask, anims);
        }
        if let Some(a) = entity_anims {
            commands.entity(entity).insert(a);
        }
    }
}

/// Spawn one character glTF as a child holder of `parent`, tagged with its slot
/// mask + animation data + ready observer. Identity transform, so it inherits the
/// avatar's pose and scale exactly.
fn spawn_slot_holder(
    commands: &mut Commands,
    asset_server: &AssetServer,
    parent: Entity,
    scene: &'static str,
    mask: SlotMask,
    anims: AvatarAnimations,
) {
    commands
        .spawn((
            anims,
            mask,
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
    let current = &store.data.appearance;
    if last.as_ref() == Some(current) {
        return;
    }
    let first_run = last.is_none();
    *last = Some(current.clone());
    if first_run {
        return; // don't rebuild the just-spawned avatar on startup
    }
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

/// Character-glTF accessory nodes to hide on spawn (weapons the avatar shouldn't
/// carry). Matched against the glTF node name.
fn is_weapon_node(name: &str) -> bool {
    matches!(
        name,
        "Pistol" | "Sword" | "Rifle" | "Gun" | "Knife" | "Axe" | "Bow" | "Spear" | "Weapon"
    )
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
    masks: Query<&SlotMask>,
    animations_to_play: Query<&AvatarAnimations>,
    mut players: Query<&mut AnimationPlayer>,
) {
    let root = trigger.event_target();
    let Ok(anim) = animations_to_play.get(root) else {
        return;
    };

    // Keep only this holder's masked slots visible, and always hide weapons (e.g.
    // the Suit/Swat `Pistol`, Medieval `Sword`) — no business avatar brandishing a
    // gun. Unslotted meshes fall through and stay. `iter_descendants` walks the
    // whole subtree; hiding a node hides its mesh children too.
    let mask = masks.get(root).copied().unwrap_or(SlotMask::ALL);
    for child in children.iter_descendants(root) {
        let Ok(name) = names.get(child) else { continue };
        let hide = is_weapon_node(name.as_str())
            || classify_slot(name.as_str()).is_some_and(|s| !mask.shows(s));
        if hide {
            commands.entity(child).insert(Visibility::Hidden);
        }
    }

    // Case 1 — the character glTF embeds its own clips, so Bevy already spawned an
    // `AnimationPlayer`. Play the idle clip on it directly.
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

/// Lean the local avatar's model into flight while it is moving under its own
/// power, and stand it back up on landing.
///
/// The Quaternius "Ultimate Modular" clip set has no flight (or jump) animation
/// and the UAL library can't supply one — different rig, so its clips don't bind
/// (`docs/notes/avatar-locomotion.md`). Flying therefore played *Idle*, i.e. an
/// avatar standing to attention while it crossed the city at 32 m/s. Pitching the
/// model forward is what turns that same pose into a glide; hovering in place
/// stays upright, which is the readable difference between the two.
///
/// The lean lives on the model holders, not on the avatar entity: the avatar's own
/// transform is its authoritative position and yaw (collision, the follow camera
/// and the replicated facing all read it), and pitching *that* would tip all three.
/// Holders pivot about chest height so the feet swing back instead of the head
/// diving through the ground.
pub fn update_avatar_flight_pose(
    time: Res<Time>,
    avatar_state: Res<AvatarState>,
    avatars: Query<Entity, With<Avatar>>,
    mut holders: Query<(&ChildOf, &mut Transform), With<SlotMask>>,
    mut lean: Local<f32>,
) {
    let target = if avatar_state.is_flying && avatar_state.is_walking {
        FLIGHT_LEAN
    } else {
        0.0
    };
    let previous = *lean;
    let a = 1.0 - (-FLIGHT_LEAN_SMOOTHING * time.delta_secs()).exp();
    *lean += (target - *lean) * a;
    if (target - *lean).abs() < 1e-3 {
        *lean = target;
    }
    // Upright and already written: the holders sit at their spawn identity, so
    // there is nothing to do (the common, grounded case costs one compare).
    if *lean == 0.0 && previous == 0.0 {
        return;
    }

    let pose = flight_pose(*lean);
    for (parent, mut tf) in holders.iter_mut() {
        if !avatars.contains(parent.parent()) {
            continue; // a remote avatar's holder — flight isn't replicated
        }
        *tf = pose;
    }
}

/// The holder transform for a given lean: a pitch about the model's X axis taken
/// around a pivot at [`FLIGHT_LEAN_PIVOT_Y`], so the chest stays put and the feet
/// swing back. Identity at zero, which is what a holder spawns with.
fn flight_pose(lean: f32) -> Transform {
    let rotation = Quat::from_rotation_x(lean);
    let pivot = Vec3::Y * FLIGHT_LEAN_PIVOT_Y;
    Transform {
        translation: pivot - rotation * pivot,
        rotation,
        scale: Vec3::ONE,
    }
}

/// Phase-lock a mixed avatar's per-slot holders: force every holder's active clips
/// to the first holder's seek time, so a head from one style and a body from
/// another (separate skeletons/players that started at slightly different frames)
/// don't drift apart. No-op for a single-holder avatar (a preset or the fox).
pub fn sync_avatar_animations(
    avatars: Query<Entity, Or<(With<Avatar>, With<RemoteAvatar>)>>,
    children: Query<&Children>,
    mut players: Query<&mut AnimationPlayer>,
) {
    for avatar in avatars.iter() {
        let holder_players: Vec<Entity> = children
            .iter_descendants(avatar)
            .filter(|&e| players.get(e).is_ok())
            .collect();
        if holder_players.len() < 2 {
            continue;
        }
        let seeks: Vec<(AnimationNodeIndex, f32)> = {
            let Ok(leader) = players.get(holder_players[0]) else {
                continue;
            };
            leader
                .playing_animations()
                .map(|(&node, active)| (node, active.seek_time()))
                .collect()
        };
        for &follower in &holder_players[1..] {
            if let Ok(mut player) = players.get_mut(follower) {
                for &(node, t) in &seeks {
                    if let Some(active) = player.animation_mut(node) {
                        active.seek_to(t);
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
        // Client-side prediction: advance the display by the velocity we're
        // sending to the sim (frame-rate integration), then reconcile toward the
        // authoritative position. `predicted_velocity` already reflects collision
        // steering.
        //
        // The reconcile target is the last snapshot **extrapolated by its own
        // age**, not the raw snapshot. `AvatarState::position` only advances at
        // the 20 Hz tick, so it is a staircase: reconciling against it directly
        // meant that between two snapshots the prediction ran forward at v while
        // the correction pulled it back toward a target that was standing still.
        // The pull reaches ONLINE_RECONCILE_RATE · v · 0.05 ≈ 3.6 m/s against a
        // 8 m/s walk just before each snapshot lands, then resets — a 20 Hz
        // sawtooth in the apparent speed, and (since the follow camera tracks
        // `display_position`) in the whole view. Extrapolating the target by
        // `snapshot_age` compares like with like: the error signal is smooth, so
        // reconciliation only corrects genuine divergence.
        avatar_state.snapshot_age =
            (avatar_state.snapshot_age + dt).min(ONLINE_MAX_EXTRAPOLATION);
        let mut target =
            avatar_state.position + avatar_state.predicted_velocity * avatar_state.snapshot_age;
        let mut display = avatar_state.display_position + avatar_state.predicted_velocity * dt;
        // Match the sim's ground clamp (`state::step`, y >= 0) so a fly-down
        // prediction doesn't dip through the floor and bounce back.
        if display.y < 0.0 {
            display.y = 0.0;
        }
        if target.y < 0.0 {
            target.y = 0.0;
        }
        avatar_state.display_position = if display.distance(target) > ONLINE_SNAP_ERROR {
            // Teleport / correction / resumed stream: ease-in would visibly slide
            // the avatar across the map instead of arriving.
            target
        } else {
            let reconcile = 1.0 - (-ONLINE_RECONCILE_RATE * dt).exp();
            display.lerp(target, reconcile)
        };
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
    mut avatar_query: Query<(&mut Transform, &big_space::prelude::GridCell), With<Avatar>>,
    mut avatar_state: ResMut<AvatarState>,
    camera_state: Res<crate::resources::CameraState>,
    egui_manager: Res<crate::systems::egui_manager::EguiManager>,
    edit_dialog: Res<crate::resources::EditDialogState>,
    colliders: Res<crate::systems::collision::BuildingColliders>,
    layers: Res<crate::systems::data_layers::DataLayers>,
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

    let Ok((mut transform, grid_cell)) = avatar_query.single_mut() else {
        return;
    };
    let delta_time = time.delta().as_secs_f32();

    let move_forward =
        keyboard_input.pressed(KeyCode::KeyW) || keyboard_input.pressed(KeyCode::ArrowUp);
    let move_backward =
        keyboard_input.pressed(KeyCode::KeyS) || keyboard_input.pressed(KeyCode::ArrowDown);
    let move_left =
        keyboard_input.pressed(KeyCode::KeyA) || keyboard_input.pressed(KeyCode::ArrowLeft);
    let move_right =
        keyboard_input.pressed(KeyCode::KeyD) || keyboard_input.pressed(KeyCode::ArrowRight);
    // Same vertical keys as the Fly camera (`free_camera`): E/Space up, Q down.
    // Shift used to mean down here, which was unambiguous in avatar flight but
    // disagreed with the camera, where Shift is the speed boost — one hand, two
    // meanings for the same key depending on the mode.
    let fly_up = keyboard_input.pressed(KeyCode::Space) || keyboard_input.pressed(KeyCode::KeyE);
    let fly_down = keyboard_input.pressed(KeyCode::KeyQ);
    if keyboard_input.just_pressed(KeyCode::KeyF) {
        avatar_state.is_flying = !avatar_state.is_flying;
    }

    let az = camera_state.azimuth;
    let wish = wish_dir_camera_relative(az, move_forward, move_backward, move_left, move_right);
    avatar_state.is_walking = wish.length_squared() > 1e-8;
    // Face the way we're travelling, in both run modes. The wish direction — not
    // the camera — is the target, so A/D turn the avatar a quarter turn and S
    // brings it about; only the turn *rate* is smoothed, movement is immediate.
    if let Some(target) = yaw_from_dir(wish) {
        avatar_state.facing_yaw = advance_facing(avatar_state.facing_yaw, target, delta_time);
    }
    // Only on a real turn: writing the same rotation back would mark the avatar's
    // Transform changed every frame and re-propagate its whole model subtree.
    let rotation = Quat::from_rotation_y(avatar_state.facing_yaw);
    if transform.rotation != rotation {
        transform.rotation = rotation;
    }

    // Online: the sim owns translation (`network::send_network_intent` sends the
    // wish dir, the fly flags and this facing); nothing below applies.
    if online.is_some() {
        return;
    }

    // Always sync avatar state position with transform
    // This ensures camera can follow the avatar correctly
    avatar_state.position = transform.translation;
    avatar_state.display_position = transform.translation;

    if avatar_state.is_walking {
        let speed = if avatar_state.is_flying { FLY_SPEED } else { WALK_SPEED };
        let delta = wish * speed * delta_time;
        // Collide against buildings while walking (anything off the ground passes
        // over roofs — the footprints are 2D, so they'd otherwise divert a flight
        // or a fall around a building it clears) when the buildings layer is on.
        // Footprints live in the anchored world-XZ frame, so resolve the move in
        // absolute metres (cell + local).
        let airborne = transform.translation.y > GROUND_HEIGHT + AIRBORNE_EPS;
        if !airborne && layers.buildings_enabled && !colliders.is_empty() {
            use crate::systems::free_camera::WORLD_CELL_EDGE;
            let base = Vec2::new(
                grid_cell.x as f32 * WORLD_CELL_EDGE,
                grid_cell.z as f32 * WORLD_CELL_EDGE,
            );
            let from = base + Vec2::new(transform.translation.x, transform.translation.z);
            let resolved = colliders.slide(from, from + Vec2::new(delta.x, delta.z));
            transform.translation.x = resolved.x - base.x;
            transform.translation.z = resolved.y - base.y;
        } else {
            transform.translation.x += delta.x;
            transform.translation.z += delta.z;
        }
    }

    // Handle vertical movement. The avatar models are authored origin-at-feet, so
    // the standing height is the ground surface itself (`GROUND_HEIGHT`) — the old
    // `+ AVATAR_HEIGHT/2` suited only the fox, whose origin is its centre, and it
    // floated the humanoids ~0.45 m.
    //
    // The vertical keys are flight controls, not levitation: an avatar that isn't
    // flying falls, whatever it is holding. Same rule as the sim
    // (`SimWorld::step`), so switching flight off in mid-air brings the avatar
    // down in either run mode.
    if avatar_state.is_flying {
        if fly_up {
            transform.translation.y += FLY_VERTICAL_SPEED * delta_time;
        } else if fly_down && transform.translation.y > GROUND_HEIGHT {
            transform.translation.y -= FLY_VERTICAL_SPEED * delta_time;
        }
    } else {
        transform.translation.y -= FALL_SPEED * delta_time;
    }
    if transform.translation.y < GROUND_HEIGHT {
        transform.translation.y = GROUND_HEIGHT;
    }

    // Update avatar state position to match transform (important for camera following)
    avatar_state.position = transform.translation;
    avatar_state.display_position = transform.translation;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turn `yaw` back into the direction the model then faces (+Z rotated by it).
    fn facing(yaw: f32) -> Vec3 {
        Quat::from_rotation_y(yaw) * Vec3::Z
    }

    fn assert_dir(actual: Vec3, expected: Vec3) {
        assert!(
            actual.distance(expected) < 1e-4,
            "expected {expected:?}, got {actual:?}"
        );
    }

    /// The yaw a direction asks for is the yaw that points the model at it — the
    /// property the whole facing chain (render transform, replicated `display_yaw`)
    /// rests on.
    #[test]
    fn yaw_faces_the_direction_it_came_from() {
        for dir in [Vec3::Z, Vec3::X, -Vec3::Z, -Vec3::X, Vec3::new(1.0, 0.0, 1.0).normalize()] {
            let yaw = yaw_from_dir(dir).expect("non-zero direction");
            assert_dir(facing(yaw), dir);
        }
        assert!(yaw_from_dir(Vec3::ZERO).is_none(), "no input must not steer");
    }

    /// Every WASD combination turns the avatar onto the direction it travels:
    /// pressing A walks *and* faces left, S about-faces. Checked against the
    /// camera's own frame so it holds at any azimuth.
    #[test]
    fn every_key_turns_the_avatar_onto_its_travel_direction() {
        for &az in &[0.0, 0.7, 2.5, -1.9] {
            let f = camera_plane_forward(az);
            let r = camera_plane_right(az);
            // (forward, backward, left, right) → expected travel/facing direction.
            let cases: [(bool, bool, bool, bool, Vec3); 5] = [
                (true, false, false, false, f),
                (false, true, false, false, -f),
                (false, false, true, false, -r),
                (false, false, false, true, r),
                (true, false, false, true, (f + r).normalize()),
            ];
            for (fwd, back, left, right, want) in cases {
                let wish = wish_dir_camera_relative(az, fwd, back, left, right);
                assert_dir(wish, want);
                let yaw = yaw_from_dir(wish).expect("a key is held");
                assert_dir(facing(yaw), want);
            }
        }
    }

    /// The turn eases rather than snapping, but it always converges — and settles
    /// exactly, so a standing avatar stops rewriting its transform.
    #[test]
    fn facing_eases_toward_the_target_and_settles() {
        let dt = 1.0 / 60.0;
        let target = std::f32::consts::PI; // the worst case: an about-face
        let mut yaw = 0.0_f32;
        let first = advance_facing(yaw, target, dt);
        assert!(first > 0.0 && first < target, "turn is eased, not a snap: {first}");
        assert!(
            first <= MAX_TURN_RATE * dt + 1e-6,
            "first frame respects the rate cap: {first}"
        );
        let mut frames = 0;
        while wrap_angle_pi(target - yaw).abs() > 1e-6 && frames < 600 {
            yaw = advance_facing(yaw, target, dt);
            frames += 1;
        }
        assert!(frames < 60, "an about-face lands within a second: {frames} frames");
        assert_eq!(
            advance_facing(yaw, target, dt),
            wrap_angle_pi(target),
            "settled, so no further movement"
        );
    }

    /// The flight lean tips the avatar *forward* — head toward its own +Z, feet
    /// back — about the chest, and is the identity when grounded. Checked here
    /// because a pose has no other way to fail loudly: a sign slip would fly the
    /// avatar on its back and only a screenshot would say so.
    #[test]
    fn flight_lean_pitches_forward_about_the_chest() {
        assert_eq!(flight_pose(0.0), Transform::IDENTITY);

        let pose = flight_pose(FLIGHT_LEAN);
        let pivot = Vec3::Y * FLIGHT_LEAN_PIVOT_Y;
        assert!(
            pose.transform_point(pivot).distance(pivot) < 1e-5,
            "the chest is the pivot, so it must not move: {:?}",
            pose.transform_point(pivot)
        );

        let head = Vec3::new(0.0, 1.7, 0.0);
        let leant = pose.transform_point(head);
        assert!(leant.z > 0.5, "head leads, toward model-forward: {leant:?}");
        assert!(leant.y < head.y, "and drops below standing height: {leant:?}");
        let feet = pose.transform_point(Vec3::ZERO);
        assert!(feet.z < 0.0, "feet trail behind: {feet:?}");
    }

    /// Turning takes the short way round the circle: from just left of −π the
    /// target just right of +π is a hair away, not a full sweep back.
    #[test]
    fn facing_turns_the_short_way_across_the_wrap() {
        let pi = std::f32::consts::PI;
        let stepped = advance_facing(pi - 0.05, -pi + 0.05, 1.0 / 60.0);
        assert!(
            stepped.abs() > pi - 0.05,
            "should step across ±π, not unwind through 0: {stepped}"
        );
    }
}
