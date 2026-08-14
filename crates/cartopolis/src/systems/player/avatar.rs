use crate::components::AvatarModelLoaded;
use crate::components::{Avatar, RemoteAvatar, RemoteAvatarMotionHint};
use crate::resources::AvatarState;
use crate::systems::locomotion;
use crate::systems::user_store::{AvatarAppearance, AvatarSex, UserStore};
use bevy::animation::graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeIndex};
use bevy::animation::transition::AnimationTransitions;
use bevy::animation::{AnimatedBy, AnimationTargetId};
use bevy::ecs::query::Or;
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use bevy::world_serialization::{WorldAssetRoot, WorldInstanceReady};
use cartopolis_core::{motion, wrap_angle_pi, Emote};
use std::time::Duration;

/// One glTF scene to spawn for an avatar, showing only the meshes for the slots in
/// `mask`. Complete Quaternius characters embed their own clips, so `anims` is the
/// same file. Several holders (different character files, different slot masks)
/// compose one mixed avatar on the shared rig.
struct Holder {
    scene: &'static str,
    anims: &'static str,
    mask: SlotMask,
    scale: f32,
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
    const ALL: Self = Self {
        head: true,
        top: true,
        legs: true,
        feet: true,
        accessory: true,
    };
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
// all embed the same 24-clip set — see `jq -r '.animations[].name'`). Verified
// against `Man_Casual.gltf`; every character file agrees, which is what makes a
// mixed avatar's holders phase-lockable at all.
const CHAR_IDLE: usize = 4;
const CHAR_WALK: usize = 22;
const CHAR_RUN: usize = 16;
// The set has no jump/fly clip, so Idle doubles as the airborne pose — what makes
// it read as flight is the forward lean in `update_avatar_flight_pose`.
const CHAR_WAVE: usize = 23;
const CHAR_INTERACT: usize = 10;
const CHAR_ROLL: usize = 15;
const CHAR_PUNCH: usize = 13;
const CHAR_KICK: usize = 11;
const CHAR_DEATH: usize = 0;

/// Emotes in the order their clips are loaded into the graph, so an [`Emote`] is
/// an index into [`AvatarAnimations::emotes`]. Adding one means a row here and a
/// clip index — the graph is built from this table.
const EMOTE_CLIPS: [(Emote, usize); 6] = [
    (Emote::Wave, CHAR_WAVE),
    (Emote::Interact, CHAR_INTERACT),
    (Emote::Roll, CHAR_ROLL),
    (Emote::Punch, CHAR_PUNCH),
    (Emote::Kick, CHAR_KICK),
    (Emote::Faint, CHAR_DEATH),
];

/// Where `emote` sits in [`EMOTE_CLIPS`].
fn emote_slot(emote: Emote) -> usize {
    EMOTE_CLIPS
        .iter()
        .position(|(e, _)| *e == emote)
        .unwrap_or(0)
}

/// A holder for one Quaternius character glTF (~1.8 m, scale 1.0; self-animated),
/// showing the slots in `mask`.
fn character_holder(scene: &'static str, mask: SlotMask) -> Holder {
    Holder {
        scene,
        anims: scene,
        mask,
        scale: 1.0,
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
        CharacterStyle {
            sex: AvatarSex::Man,
            label: $label,
            file: concat!("models/avatar/characters/", $file),
        }
    };
}
macro_rules! woman {
    ($label:literal, $file:literal) => {
        CharacterStyle {
            sex: AvatarSex::Woman,
            label: $label,
            file: concat!("models/avatar/characters/", $file),
        }
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

/// Whether an avatar glTF is present under the runtime asset root.
///
/// **Only a filesystem can be asked this, and two platforms haven't got one.**
/// This probe used to be `Path::new("assets").join(…).is_file()` — a *relative*
/// path, i.e. one resolved against the process's working directory rather than
/// against [`crate::ASSET_ROOT`], which is what actually feeds `AssetPlugin`.
/// The two coincide only when the binary is launched from the repo root, and
/// where they didn't the answer was `false` for every one of the twenty catalog
/// entries: `style_file` returned `None` for each slot, `resolve_avatar_holders`
/// returned an empty vec, and `spawn_avatar` inserted `AvatarModelLoaded` (so it
/// never retried) and spawned no meshes at all. The avatar entity still existed,
/// so Follow mode was offered, the camera followed it and it walked — it was
/// simply not drawn. That was the state of **Android** (a NativeActivity process
/// starts at `/`, and its assets live inside the APK where only the framework
/// `AssetManager` can reach them) and of the **web build** (`std::fs` on wasm32
/// is stubbed, so `is_file()` is unconditionally false), for as long as either
/// has existed.
///
/// So on those two the answer is `true` by construction: the asset store is a
/// bundle, its contents are fixed at package time, and a path in it is not a
/// path a process can stat. If a file really is missing there, the asset server
/// says so — which is the right instrument, because it names the file.
///
/// What this does **not** catch, despite reading as if it might: a clone without
/// Git LFS. A pointer file is a file, so `is_file()` is happily true and the
/// glTF parse is what fails. LFS is guarded by `.gitattributes` and by every
/// workflow's `lfs: true`, not here.
fn avatar_asset_exists(scene_rel: &str) -> bool {
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    {
        let _ = scene_rel;
        true
    }
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    {
        std::path::Path::new(crate::ASSET_ROOT)
            .join(scene_rel)
            .is_file()
    }
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

/// Offline flight's vertical rate, mirroring what the sim gives an online client
/// so the same keys move the avatar at the same rate in every run mode
/// (`cartopolis_simulator::state::FLY_VERTICAL_SPEED`). The *horizontal* rates now come
/// from `systems::locomotion`, which is shared by all three copies of the model.
const FLY_VERTICAL_SPEED: f32 = 12.0;

/// How long Space must be held to take off, rather than jump.
///
/// Space is the right key for both verbs and they cannot both be a tap, so the
/// tap goes to the one that must be instant. A jump fires on the *press*, with no
/// wait to see whether a hold is coming — holding on from there simply turns the
/// jump into a take-off, which is a better transition than either alone. Landing
/// stays a tap, since there is no competing verb in the air.
const FLIGHT_HOLD_SECS: f32 = 0.25;

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

/// Peak pitch of the jump tuck (~11°), reached at full ascent speed and mirrored
/// to a backward opening at full descent. Small on purpose: the clip underneath
/// is still `Idle`, and a large angle turns a jump into a dive.
const JUMP_TUCK: f32 = 0.2;
/// The jump/fall pose has to keep up with a 0.64 s arc, so it eases far faster
/// than the flight lean, which is settling into a cruise.
const JUMP_TUCK_SMOOTHING: f32 = 18.0;

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

/// The travel direction and the throttle it carries, from the keys and the
/// on-screen thumbstick (`systems::map_osd`) together.
///
/// Returns a unit direction on XZ — or zero for "no input" — and a `0..=1`
/// throttle. The keys are all-or-nothing and answer `1.0`; the stick is
/// proportional and answers its own push, which is the whole difference between
/// a stick and four keys (`locomotion::speed_for_throttle` turns it into a
/// speed).
///
/// **Keys win when both are live**, and the tie is not hypothetical: a desktop
/// touchscreen draws the stick *and* has a keyboard. Blending them would let a
/// thumb resting inside the deadzone quietly cap a sprint — the same failure
/// `Gait::from_modifiers` avoids by letting Sprint win its tie.
pub(crate) fn travel_wish(azimuth: f32, keys: Vec3, stick: Vec2) -> (Vec3, f32) {
    if keys.length_squared() > 1e-8 {
        return (keys, 1.0);
    }
    let push = stick.length();
    if push <= 1e-4 {
        return (Vec3::ZERO, 0.0);
    }
    let unit = stick / push;
    // Same frame as the keys: `y` is forward (up-screen), `x` strafes right.
    let dir = camera_plane_forward(azimuth) * unit.y + camera_plane_right(azimuth) * unit.x;
    (dir.normalize_or_zero(), push.min(1.0))
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

/// Standing height on flat ground: the region tile's top surface (the cuboid's
/// half-extent in y). The avatar models are authored origin-at-feet, so this *is*
/// the resting height — not a half-height offset.
///
/// Public because collision's snap-out backstop has to reconstruct the same
/// ground the movement path stands on (`GROUND_HEIGHT + terrain.at(..)`), and a
/// second copy of the number there is exactly the drift CLAUDE.md's
/// "standing height comes from THREE places" note is about.
pub const GROUND_HEIGHT: f32 = 0.05;
/// Height above the ground at which the avatar stops being a pedestrian: the
/// building footprints are 2D, so they only apply to something standing on them.
pub(crate) const AIRBORNE_EPS: f32 = 0.2;

/// Animation data attached to an avatar (and each of its model "holder" children),
/// read once a scene is ready. `Clone` so the body and outfit holders can share it.
#[derive(Component, Clone)]
pub(crate) struct AvatarAnimations {
    graph_handle: Handle<AnimationGraph>,
    idle_index: AnimationNodeIndex,
    /// The stroll. Loading it is what lets the avatar have a slow gait at all —
    /// the client played `Run` at every speed before, which is why the feet slid.
    walk_index: AnimationNodeIndex,
    run_index: AnimationNodeIndex,
    fly_index: AnimationNodeIndex,
    /// One node per [`EMOTE_CLIPS`] row, in that order.
    emote_indices: Vec<AnimationNodeIndex>,
    /// Base playback speed for this model's clips (`Holder::anim_speed`). The
    /// locomotion clips are scaled on top of this to match ground speed.
    speed: f32,
}

/// A one-shot emote currently playing on an avatar. Present only while it runs;
/// [`update_avatar_animation`] removes it when the clip is done or movement
/// interrupts it.
#[derive(Component)]
pub(crate) struct EmotePlayback {
    emote: Emote,
    remaining: f32,
}

/// How long an emote holds the body before locomotion takes over again. The clips
/// run 1.27–1.67 s; a single duration for all of them is enough because the
/// transition back cross-fades anyway, and it saves reading every clip's length
/// out of the asset at runtime.
const EMOTE_DURATION: f32 = 1.6;

/// Cross-fade time between locomotion clips. Long enough to hide the pop that
/// `stop`/`play` used to make on every start and stop, short enough that the
/// avatar does not appear to change its mind — a gait change is a decision, not a
/// drift.
const GAIT_FADE: Duration = Duration::from_millis(180);
/// Emotes cut in faster: a wave that eases in over a fifth of a second reads as
/// reluctance.
const EMOTE_FADE: Duration = Duration::from_millis(90);

pub fn spawn_avatar(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    store: Res<UserStore>,
    mut avatar_query: Query<
        (Entity, Has<Avatar>, &mut Transform),
        (
            Without<AvatarModelLoaded>,
            Or<(With<Avatar>, With<RemoteAvatar>)>,
        ),
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
            // Locomotion first, then the emotes in `EMOTE_CLIPS` order, so an
            // `Emote` indexes `emote_indices` directly.
            let mut clips: Vec<Handle<AnimationClip>> = vec![
                asset_server.load(GltfAssetLabel::Animation(CHAR_IDLE).from_asset(h.anims)),
                asset_server.load(GltfAssetLabel::Animation(CHAR_WALK).from_asset(h.anims)),
                asset_server.load(GltfAssetLabel::Animation(CHAR_RUN).from_asset(h.anims)),
            ];
            clips.extend(EMOTE_CLIPS.iter().map(|(_, ix)| {
                asset_server.load(GltfAssetLabel::Animation(*ix).from_asset(h.anims))
            }));
            let (graph, indices) = AnimationGraph::from_clips(clips);
            let anims = AvatarAnimations {
                graph_handle: graphs.add(graph),
                idle_index: indices[0],
                walk_index: indices[1],
                run_index: indices[2],
                // Flight has no clip of its own; the forward lean in
                // `update_avatar_flight_pose` is what distinguishes it from
                // standing (`docs/notes/avatar-locomotion.md`).
                fly_index: indices[0],
                emote_indices: indices[3..].to_vec(),
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
        // One line per avatar spawn, and it is an instrument rather than chatter:
        // `holders = 0` is precisely the state this commit fixes, and it is
        // otherwise indistinguishable from a healthy spawn — nothing downstream
        // complains about an avatar with no meshes. The character glTFs are also
        // the only thing this client loads through the asset server on Android,
        // where the log is the only instrument there is
        // (`docs/notes/android-build.md`).
        tracing::info!(
            local = is_local,
            holders = holders.len(),
            scenes = ?holders.iter().map(|h| h.scene).collect::<Vec<_>>(),
            "avatar: model requested"
        );
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
    let (mut nodes, mut hidden) = (0usize, 0usize);
    for child in children.iter_descendants(root) {
        nodes += 1;
        let Ok(name) = names.get(child) else { continue };
        let hide = is_weapon_node(name.as_str())
            || classify_slot(name.as_str()).is_some_and(|s| !mask.shows(s));
        if hide {
            hidden += 1;
            commands.entity(child).insert(Visibility::Hidden);
        }
    }
    // The other half of the bracket opened in `spawn_avatar` — see its note. This
    // one also carries the mask's own arithmetic, because "the scene loaded and
    // every node in it was masked away" and "the scene never loaded" look
    // identical from the outside and are nothing alike.
    tracing::info!(nodes, hidden, "avatar: model ready");

    // Case 1 — the character glTF embeds its own clips, so Bevy already spawned an
    // `AnimationPlayer`. Play the idle clip on it directly.
    if let Some(player_entity) = children
        .iter_descendants(root)
        .find(|&e| players.get(e).is_ok())
    {
        if let Ok(mut player) = players.get_mut(player_entity) {
            // Every clip change goes through `AnimationTransitions` from here on,
            // so the idle has to be started through it too — driving the player
            // directly leaves the transitions component with no idea what the
            // "main" animation is, and the first cross-fade then fades from
            // nothing.
            let mut transitions = AnimationTransitions::new();
            transitions
                .play(&mut player, anim.idle_index, Duration::ZERO)
                .repeat()
                .set_speed(anim.speed);
            commands
                .entity(player_entity)
                .insert((AnimationGraphHandle(anim.graph_handle.clone()), transitions));
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
    bind_external_animations(
        &mut commands,
        armature,
        armature,
        &mut path,
        &children,
        &names,
    );

    let mut player = AnimationPlayer::default();
    let mut transitions = AnimationTransitions::new();
    transitions
        .play(&mut player, anim.idle_index, Duration::ZERO)
        .repeat()
        .set_speed(anim.speed);
    commands.entity(armature).insert((
        player,
        transitions,
        AnimationGraphHandle(anim.graph_handle.clone()),
    ));
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
    commands.entity(entity).insert((
        AnimationTargetId::from_names(path.iter()),
        AnimatedBy(player),
    ));
    if let Ok(kids) = children.get(entity) {
        for child in kids.iter() {
            bind_external_animations(commands, child, player, path, children, names);
        }
    }
    path.pop();
}

/// What an avatar's body is doing this frame: which clip, how fast, and whether
/// it loops. Derived from ground speed rather than from a boolean, which is the
/// whole of the fix — the clip used to be chosen by "is there input" and played at
/// a fixed 1.0×, so the legs ran at the animator's speed while the ground moved at
/// whatever the sim said.
#[derive(Clone, Copy, PartialEq)]
struct Pose {
    clip: PoseClip,
    /// Playback rate multiplier, before the model's own `anim_speed`.
    rate: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PoseClip {
    Idle,
    Walk,
    Run,
    Fly,
    Emote(usize),
}

/// Choose the locomotion pose for a ground speed. `running` is the previous
/// answer, for the hysteresis in `locomotion::wants_run_clip`.
fn locomotion_pose(speed: f32, flying: bool, running: bool) -> Pose {
    if flying {
        return Pose {
            clip: PoseClip::Fly,
            rate: 1.0,
        };
    }
    if speed < locomotion::MOVING_EPS {
        return Pose {
            clip: PoseClip::Idle,
            rate: 1.0,
        };
    }
    if locomotion::wants_run_clip(speed, running) {
        Pose {
            clip: PoseClip::Run,
            rate: locomotion::clip_speed_for(speed, locomotion::RUN_CLIP_SPEED),
        }
    } else {
        Pose {
            clip: PoseClip::Walk,
            rate: locomotion::clip_speed_for(speed, locomotion::WALK_CLIP_SPEED),
        }
    }
}

impl AvatarAnimations {
    fn node(&self, clip: PoseClip) -> AnimationNodeIndex {
        match clip {
            PoseClip::Idle => self.idle_index,
            PoseClip::Walk => self.walk_index,
            PoseClip::Run => self.run_index,
            PoseClip::Fly => self.fly_index,
            PoseClip::Emote(slot) => self
                .emote_indices
                .get(slot)
                .copied()
                .unwrap_or(self.idle_index),
        }
    }
}

/// Drive one player to `pose`, cross-fading if the clip changed and only
/// adjusting the rate if it didn't.
///
/// The rate is re-applied every frame *without* restarting anything: it tracks a
/// continuously ramping ground speed, so it changes on almost every frame of an
/// acceleration, and calling `play` for that would restart the stride each time.
fn apply_pose(
    player: &mut AnimationPlayer,
    transitions: &mut AnimationTransitions,
    anim: &AvatarAnimations,
    pose: Pose,
) {
    let want = anim.node(pose.clip);
    let repeat = !matches!(pose.clip, PoseClip::Emote(_));
    if transitions.get_main_animation() != Some(want) {
        let fade = if repeat { GAIT_FADE } else { EMOTE_FADE };
        let active = transitions.play(player, want, fade);
        if repeat {
            active.repeat();
        }
    }
    if let Some(active) = player.animation_mut(want) {
        active.set_speed(anim.speed * pose.rate);
    }
}

/// Switch the local avatar's animation to match what it is actually doing:
/// standing, strolling, running (at the speed it is running), flying, or playing
/// an emote.
pub fn update_avatar_animation(
    mut commands: Commands,
    time: Res<Time>,
    mut avatar_state: ResMut<AvatarState>,
    children: Query<&Children>,
    animations_to_play: Query<&AvatarAnimations, With<Avatar>>,
    mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
    mut avatar_query: Query<(Entity, Option<&mut EmotePlayback>), With<Avatar>>,
    mut running: Local<bool>,
) {
    let dt = time.delta_secs();
    let moving = avatar_state.ground_speed >= locomotion::MOVING_EPS;
    let started = avatar_state.pending_emote.take();

    for (avatar_entity, emote_playback) in avatar_query.iter_mut() {
        // An emote owns the body until it finishes — or until the avatar moves,
        // which cancels it. Standing still to wave is a rule players already know,
        // and it avoids a wave sliding across the pavement.
        let mut active_emote = None;
        match (started, emote_playback) {
            (Some(e), Some(mut playback)) => {
                playback.emote = e;
                playback.remaining = EMOTE_DURATION;
                active_emote = Some(e);
            }
            (Some(e), None) => {
                commands.entity(avatar_entity).insert(EmotePlayback {
                    emote: e,
                    remaining: EMOTE_DURATION,
                });
                active_emote = Some(e);
            }
            (None, Some(mut playback)) => {
                playback.remaining -= dt;
                if playback.remaining <= 0.0 || moving {
                    commands.entity(avatar_entity).remove::<EmotePlayback>();
                } else {
                    active_emote = Some(playback.emote);
                }
            }
            (None, None) => {}
        }

        let pose = match active_emote {
            Some(e) => Pose {
                clip: PoseClip::Emote(emote_slot(e)),
                rate: 1.0,
            },
            None => locomotion_pose(avatar_state.ground_speed, avatar_state.is_flying, *running),
        };
        *running = pose.clip == PoseClip::Run;

        let Ok(anim) = animations_to_play.get(avatar_entity) else {
            continue;
        };
        for child in children.iter_descendants(avatar_entity) {
            if let Ok((mut player, mut transitions)) = players.get_mut(child) {
                apply_pose(&mut player, &mut transitions, anim, pose);
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
    } else if !avatar_state.is_flying && !avatar_state.grounded {
        // A jump has no clip either (same 24-clip set, same gap), so it gets the
        // same treatment as flight at a much smaller angle: the body tucks forward
        // on the way up and opens out as it falls, driven by the vertical velocity
        // itself. Signed, so the rise and the fall do not look identical — the
        // give-away that an "airborne" pose is really a single frozen frame.
        (-avatar_state.vertical_velocity / motion::JUMP_SPEED).clamp(-1.0, 1.0) * JUMP_TUCK
    } else {
        0.0
    };
    let previous = *lean;
    let rate = if avatar_state.is_flying {
        FLIGHT_LEAN_SMOOTHING
    } else {
        JUMP_TUCK_SMOOTHING
    };
    let a = 1.0 - (-rate * time.delta_secs()).exp();
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
    // Reused across frames and avatars. A preset avatar — all four slots from one
    // style — resolves to a *single* holder, which is the common case and the one
    // that leaves here having done nothing; it should not also cost two heap
    // allocations per frame for the privilege.
    mut holder_players: Local<Vec<Entity>>,
    mut seeks: Local<Vec<(AnimationNodeIndex, f32)>>,
) {
    for avatar in avatars.iter() {
        holder_players.clear();
        holder_players.extend(
            children
                .iter_descendants(avatar)
                .filter(|&e| players.get(e).is_ok()),
        );
        if holder_players.len() < 2 {
            continue;
        }
        seeks.clear();
        {
            let Ok(leader) = players.get(holder_players[0]) else {
                continue;
            };
            seeks.extend(
                leader
                    .playing_animations()
                    .map(|(&node, active)| (node, active.seek_time())),
            );
        }
        for &follower in &holder_players[1..] {
            if let Ok(mut player) = players.get_mut(follower) {
                for &(node, t) in seeks.iter() {
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

/// Walk / run / idle / emote for other players' avatars.
///
/// Their speed is *measured* from the smoothed motion rather than replicated
/// (`RemoteAvatarMotionHint::horizontal_speed`), which is enough to pick the clip
/// and its rate — so a remote avatar strolling looks like it is strolling, by the
/// same rule the local one follows. Flight is still not replicated, so a remote
/// never takes the fly pose.
pub fn update_remote_avatar_animation(
    mut commands: Commands,
    online: Option<Res<crate::resources::OnlineSession>>,
    time: Res<Time>,
    children: Query<&Children>,
    animations_to_play: Query<&AvatarAnimations, With<RemoteAvatar>>,
    hints: Query<&RemoteAvatarMotionHint, With<RemoteAvatar>>,
    mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
    mut remote_roots: Query<
        (Entity, &RemoteAvatar, Option<&mut EmotePlayback>),
        With<RemoteAvatar>,
    >,
    mut running: Local<std::collections::HashMap<Entity, bool>>,
) {
    if online.is_none() {
        return;
    }
    let dt = time.delta_secs();
    for (avatar_entity, remote, emote_playback) in remote_roots.iter_mut() {
        let Ok(anim) = animations_to_play.get(avatar_entity) else {
            continue;
        };
        let speed = hints
            .get(avatar_entity)
            .map_or(0.0, |h| h.horizontal_speed)
            // Below the old run threshold the measurement is mostly interpolation
            // noise, so treat it as standing rather than as a very slow walk.
            .max(0.0);
        let moving = speed > REMOTE_RUN_SPEED_THRESH;

        // `RemoteAvatar::pending_emote` is set by the snapshot apply when it sees a
        // sequence it hasn't played yet, so this is an edge, not a level.
        let mut active_emote = None;
        match (remote.pending_emote, emote_playback) {
            (Some(e), Some(mut playback)) => {
                playback.emote = e;
                playback.remaining = EMOTE_DURATION;
                active_emote = Some(e);
            }
            (Some(e), None) => {
                commands.entity(avatar_entity).insert(EmotePlayback {
                    emote: e,
                    remaining: EMOTE_DURATION,
                });
                active_emote = Some(e);
            }
            (None, Some(mut playback)) => {
                playback.remaining -= dt;
                if playback.remaining <= 0.0 || moving {
                    commands.entity(avatar_entity).remove::<EmotePlayback>();
                } else {
                    active_emote = Some(playback.emote);
                }
            }
            (None, None) => {}
        }

        let was_running = running.get(&avatar_entity).copied().unwrap_or(false);
        let pose = match active_emote {
            Some(e) => Pose {
                clip: PoseClip::Emote(emote_slot(e)),
                rate: 1.0,
            },
            None => locomotion_pose(if moving { speed } else { 0.0 }, false, was_running),
        };
        running.insert(avatar_entity, pose.clip == PoseClip::Run);

        for child in children.iter_descendants(avatar_entity) {
            if let Ok((mut player, mut transitions)) = players.get_mut(child) {
                apply_pose(&mut player, &mut transitions, anim, pose);
            }
        }
    }
}

/// Clear the emote edge once every consumer has seen it (this runs after
/// [`update_remote_avatar_animation`] in the schedule). Kept separate so the
/// animation system can take `&RemoteAvatar` immutably alongside its other
/// queries.
pub fn clear_remote_emote_edges(mut remotes: Query<&mut RemoteAvatar>) {
    for mut remote in remotes.iter_mut() {
        if remote.pending_emote.is_some() {
            remote.pending_emote = None;
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
    terrain: Res<crate::systems::terrain::TerrainField>,
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
        avatar_state.snapshot_age = (avatar_state.snapshot_age + dt).min(ONLINE_MAX_EXTRAPOLATION);
        let mut target =
            avatar_state.position + avatar_state.predicted_velocity * avatar_state.snapshot_age;
        let mut display = avatar_state.display_position + avatar_state.predicted_velocity * dt;
        // The sim has no terrain and never will — it holds no map data at all, and
        // the map streams only on the client. So the ground is added *here*, on the
        // display side: the wire, the prediction and the reconciliation all stay on
        // the flat plane the sim actually simulates, and only what you see stands on
        // the measured one. Last frame's lift comes off first for exactly that
        // reason — smoothing a lifted position toward a flat target would drag the
        // avatar back into the hillside every frame, and the two would fight for as
        // long as you stood on a slope.
        display.y -= avatar_state.terrain_lift;
        // Match the sim's ground clamp (`state::step`, y >= 0) so a fly-down
        // prediction doesn't dip through the floor and bounce back.
        if display.y < 0.0 {
            display.y = 0.0;
        }
        if target.y < 0.0 {
            target.y = 0.0;
        }
        let flat = if display.distance(target) > ONLINE_SNAP_ERROR {
            // Teleport / correction / resumed stream: ease-in would visibly slide
            // the avatar across the map instead of arriving.
            target
        } else {
            let reconcile = 1.0 - (-ONLINE_RECONCILE_RATE * dt).exp();
            display.lerp(target, reconcile)
        };
        avatar_state.terrain_lift = terrain.at(flat.x, flat.z);
        avatar_state.display_position = flat + Vec3::Y * avatar_state.terrain_lift;
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
    // The on-screen thumbstick — a finger's WASD. Written by
    // `map_osd::render_map_osd`, which runs *after* this system (see its doc), so
    // this is last frame's push: a constant frame of latency on a held
    // translation control, not jitter.
    osd: Res<crate::systems::map_osd::OsdInput>,
    time: Res<Time>,
    mut avatar_query: Query<(&mut Transform, &big_space::prelude::GridCell), With<Avatar>>,
    mut avatar_state: ResMut<AvatarState>,
    camera_state: Res<crate::resources::CameraState>,
    egui_manager: Res<crate::systems::egui_manager::EguiManager>,
    edit_dialog: Res<crate::resources::EditDialogState>,
    colliders: Res<crate::systems::collision::BuildingColliders>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    // How long Space has been held this press, and whether the hold has already
    // been spent on a flight toggle (so it fires once, not every frame past the
    // threshold).
    mut space_held: Local<f32>,
    mut flight_armed: Local<bool>,
) {
    // Don't move avatar unless the follow camera is active (Free and Earth
    // modes decouple the camera from the avatar).
    if camera_state.mode != crate::resources::CameraMode::Avatar {
        avatar_state.is_walking = false;
        // Let the avatar come to rest rather than freezing mid-stride: leaving
        // the speed latched would have it resume at full pace on returning to
        // Follow, and (online) keep sending that speed as intent.
        avatar_state.ground_speed = 0.0;
        return;
    }
    // Don't move while the UI owns the keyboard (text field focused, edit dialog open,
    // or a Ctrl chord like Ctrl+Shift+A); otherwise WASD/S leak into avatar movement.
    if egui_manager.ui_owns_keyboard(edit_dialog.visible, &keyboard_input) {
        avatar_state.is_walking = false;
        avatar_state.ground_speed = 0.0;
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
    // Same vertical keys as the Fly camera (`free_camera`): R up, Q down, and
    // nothing else. Shift used to mean down here, which was unambiguous in avatar
    // flight but disagreed with the camera, where Shift is the speed boost — one
    // hand, two meanings for the same key depending on the mode. E was the climb
    // key until the world gained things to interact with; E is the near-universal
    // "use" binding and a climb key is the cheaper of the two to move.
    // The on-screen climb pair stands in for R/Q, and only in flight — that is
    // the only time Follow draws it (`map_osd`), and the only time the vertical
    // keys do anything either.
    let fly_up = keyboard_input.pressed(KeyCode::KeyR) || osd.climb > 0.0;
    let fly_down = keyboard_input.pressed(KeyCode::KeyQ) || osd.climb < 0.0;
    // Gait modifiers. Shift sprints, Alt strolls, neither jogs — see
    // `systems::locomotion`. Shift is free in Follow (the camera's boost is a Fly
    // binding) and Alt is unused anywhere in this client.
    let sprint =
        keyboard_input.pressed(KeyCode::ShiftLeft) || keyboard_input.pressed(KeyCode::ShiftRight);
    let walk_mod =
        keyboard_input.pressed(KeyCode::AltLeft) || keyboard_input.pressed(KeyCode::AltRight);
    avatar_state.gait = locomotion::Gait::from_modifiers(sprint, walk_mod);

    // Space is jump *and* take-off, resolved by how long it is held: the press
    // jumps at once (a jump that waits to see whether a hold is coming is not a
    // jump), and holding on from there turns that jump into a take-off. Tapping it
    // in the air lands. F remains the unambiguous flight toggle.
    let space = keyboard_input.pressed(KeyCode::Space);
    if keyboard_input.just_pressed(KeyCode::Space) {
        *space_held = 0.0;
        if avatar_state.is_flying {
            avatar_state.is_flying = false; // tap to land
            *flight_armed = true; // …and don't also take off on the hold
        } else {
            avatar_state.wants_jump = true;
            *flight_armed = false;
        }
    } else if space && !*flight_armed {
        *space_held += delta_time;
        if *space_held >= FLIGHT_HOLD_SECS {
            avatar_state.is_flying = true;
            *flight_armed = true;
        }
    }
    if !space {
        *flight_armed = false;
        *space_held = 0.0;
    }
    if keyboard_input.just_pressed(KeyCode::KeyF) || osd.fly_toggle {
        avatar_state.is_flying = !avatar_state.is_flying;
    }
    // The on-screen jump, which the stick has no way to express. Grounded-only
    // for the same reason `SimWorld::step` grants it that way — holding the bit
    // must not climb — and taken as an edge, since `map_osd` sets it for the one
    // frame the button was released on.
    if osd.jump && avatar_state.grounded && !avatar_state.is_flying {
        avatar_state.wants_jump = true;
    }

    let az = camera_state.azimuth;
    // The keys and the on-screen stick are one input with two shapes: a
    // direction, and how hard it is being asked for. The keyboard's throttle is
    // always 1 and its *rung* comes from the modifiers; the stick has no
    // modifiers to hold, so its push carries the choice instead
    // (`locomotion::speed_for_throttle`).
    let keys = wish_dir_camera_relative(az, move_forward, move_backward, move_left, move_right);
    let from_keys = keys.length_squared() > 1e-8;
    let (wish, throttle) = travel_wish(az, keys, osd.walk);
    let has_input = wish.length_squared() > 1e-8;
    let walk_speed = if from_keys {
        avatar_state.gait.speed()
    } else {
        locomotion::speed_for_throttle(throttle)
    };
    avatar_state.is_walking = has_input;
    // Face the way we're travelling, in both run modes. The wish direction — not
    // the camera — is the target, so A/D turn the avatar a quarter turn and S
    // brings it about; only the turn *rate* is smoothed, movement is immediate.
    if let Some(target) = yaw_from_dir(wish) {
        avatar_state.facing_yaw = advance_facing(avatar_state.facing_yaw, target, delta_time);
    }

    // ── The speed model ──────────────────────────────────────────────────────
    //
    // Ramp a *speed*, and keep the direction across a key release. Both halves
    // matter: assigning the target speed outright is what made the avatar start and
    // stop like a mechanism, and zeroing the direction with the input is what made
    // it stop dead — the wish vector is already zero on the frame the key comes up,
    // so there is nothing left to coast along unless it is remembered here.
    //
    // This runs in *every* run mode, online included, because it is the one
    // movement system that always runs in Follow. `send_network_intent` consumes
    // the result rather than recomputing it, so there is exactly one ramp.
    if has_input {
        avatar_state.move_dir = wish;
    }
    let (target_speed, accel, decel) = if avatar_state.is_flying {
        (
            if has_input {
                locomotion::FLY_SPEED
            } else {
                0.0
            },
            locomotion::FLY_ACCEL,
            locomotion::FLY_DECEL,
        )
    } else {
        (
            if has_input { walk_speed } else { 0.0 },
            locomotion::GROUND_ACCEL,
            locomotion::GROUND_DECEL,
        )
    };
    avatar_state.ground_speed = locomotion::advance_speed(
        avatar_state.ground_speed,
        target_speed,
        delta_time,
        accel,
        decel,
    );
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

    if avatar_state.ground_speed > 1e-4 {
        let delta = avatar_state.move_dir * avatar_state.ground_speed * delta_time;
        // Collide against buildings while walking (anything off the ground passes
        // over roofs — the footprints are 2D, so they'd otherwise divert a flight
        // or a fall around a building it clears) when the buildings layer is on.
        // Footprints live in the anchored world-XZ frame, so resolve the move in
        // absolute metres (cell + local).
        let airborne = transform.translation.y
            > GROUND_HEIGHT
                + terrain.at(transform.translation.x, transform.translation.z)
                + AIRBORNE_EPS;
        if !airborne && layers.buildings_enabled && !colliders.is_empty() {
            use crate::grid::WORLD_CELL_EDGE;
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
    // flying is on the gravity arc, whatever it is holding. Same function as the
    // sim (`motion::step_fall`), so switching flight off in mid-air brings the
    // avatar down at the same rate in either run mode.
    // Offline, this transform *is* the avatar's state, so the terrain enters as the
    // ground level itself rather than as a display offset the way it does online
    // (`smooth_online_avatar_display`): lifting the transform after the fact would
    // read as airborne on the next frame and gravity would take it straight back.
    let ground_y = GROUND_HEIGHT + terrain.at(transform.translation.x, transform.translation.z);
    if avatar_state.is_flying {
        if fly_up {
            transform.translation.y += FLY_VERTICAL_SPEED * delta_time;
        } else if fly_down && transform.translation.y > ground_y {
            transform.translation.y -= FLY_VERTICAL_SPEED * delta_time;
        }
        transform.translation.y = transform.translation.y.max(ground_y);
        avatar_state.vertical_velocity = 0.0;
        avatar_state.grounded = false;
    } else {
        if avatar_state.wants_jump && avatar_state.grounded {
            avatar_state.vertical_velocity = motion::JUMP_SPEED;
        }
        let step = motion::step_fall(
            transform.translation.y,
            avatar_state.vertical_velocity,
            ground_y,
            delta_time,
        );
        transform.translation.y = step.y;
        avatar_state.vertical_velocity = step.vertical_velocity;
        avatar_state.grounded = step.grounded;
        avatar_state.landing_impact = step.landing_speed.unwrap_or(0.0);
    }
    // Consumed either way: a jump asked for in mid-air is not banked until landing.
    avatar_state.wants_jump = false;

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
        for dir in [
            Vec3::Z,
            Vec3::X,
            -Vec3::Z,
            -Vec3::X,
            Vec3::new(1.0, 0.0, 1.0).normalize(),
        ] {
            let yaw = yaw_from_dir(dir).expect("non-zero direction");
            assert_dir(facing(yaw), dir);
        }
        assert!(
            yaw_from_dir(Vec3::ZERO).is_none(),
            "no input must not steer"
        );
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

    /// The default appearance resolves to a model that will actually be spawned.
    ///
    /// This is the test that was missing, and it fails against the old
    /// `avatar_asset_exists` *in this very binary*: cargo runs a test from the
    /// package directory, so the old relative `assets/` probe resolved to
    /// `crates/cartopolis/assets`, found nothing, and every style read as absent
    /// — exactly the state Android and the web build were in permanently. An
    /// empty holder list is not a loud failure anywhere: `spawn_avatar` marks the
    /// avatar loaded and spawns no meshes, so the entity is present, followable
    /// and walkable, and invisible.
    #[test]
    fn the_default_appearance_resolves_to_a_model() {
        let holders = resolve_avatar_holders(&AvatarAppearance::default());
        assert!(
            !holders.is_empty(),
            "the default avatar resolved to no model at all — it would spawn invisible"
        );
    }

    /// Every style in the chooser is backed by a file that resolves, so picking
    /// one cannot silently leave the avatar with nothing to draw.
    #[test]
    fn every_catalog_style_resolves() {
        for c in CHARACTER_STYLES {
            assert!(
                style_file(c.sex, c.label).is_some(),
                "catalog style {:?} ({}) does not resolve — the chooser would offer an empty avatar",
                c.label,
                c.file
            );
        }
    }

    /// The stick steers in the same camera frame the keys do, at any azimuth:
    /// pushing it up-screen is W, pushing it right is D. If these two ever
    /// disagreed the avatar would walk one way on a desk and another on a phone.
    #[test]
    fn the_stick_steers_in_the_same_frame_as_the_keys() {
        for &az in &[0.0, 0.7, 2.5, -1.9] {
            let f = camera_plane_forward(az);
            let r = camera_plane_right(az);
            for (stick, want) in [
                (Vec2::new(0.0, 1.0), f),
                (Vec2::new(0.0, -1.0), -f),
                (Vec2::new(1.0, 0.0), r),
                (Vec2::new(-1.0, 0.0), -r),
                (Vec2::new(1.0, 1.0).normalize(), (f + r).normalize()),
            ] {
                let (dir, _) = travel_wish(az, Vec3::ZERO, stick);
                assert_dir(dir, want);
                // …and the avatar turns onto it, exactly as a key makes it.
                assert_dir(facing(yaw_from_dir(dir).expect("pushed")), want);
            }
        }
    }

    /// The push is a throttle, not a switch: the direction is unit whatever the
    /// deflection, and the magnitude comes back beside it for the speed model.
    #[test]
    fn the_stick_reports_direction_and_push_separately() {
        let (dir, push) = travel_wish(0.0, Vec3::ZERO, Vec2::new(0.0, 0.4));
        assert!((dir.length() - 1.0).abs() < 1e-4, "unit direction: {dir:?}");
        assert!((push - 0.4).abs() < 1e-5, "push carried through: {push}");
        // Clamped to the disc even if a caller hands over a longer vector.
        let (_, over) = travel_wish(0.0, Vec3::ZERO, Vec2::new(3.0, 4.0));
        assert!((over - 1.0).abs() < 1e-5, "{over}");
        // A resting thumb is no input at all — not a slow crawl.
        assert_eq!(travel_wish(0.0, Vec3::ZERO, Vec2::ZERO), (Vec3::ZERO, 0.0));
    }

    /// Keys beat the stick, and at full throttle, so a thumb resting inside the
    /// deadzone (or an OSD frame that arrived late) cannot cap a sprint.
    #[test]
    fn the_keys_win_the_tie_at_full_throttle() {
        let keys = wish_dir_camera_relative(0.0, true, false, false, false);
        let (dir, push) = travel_wish(0.0, keys, Vec2::new(0.9, -0.9));
        assert_dir(dir, keys);
        assert_eq!(push, 1.0);
    }

    /// The turn eases rather than snapping, but it always converges — and settles
    /// exactly, so a standing avatar stops rewriting its transform.
    #[test]
    fn facing_eases_toward_the_target_and_settles() {
        let dt = 1.0 / 60.0;
        let target = std::f32::consts::PI; // the worst case: an about-face
        let mut yaw = 0.0_f32;
        let first = advance_facing(yaw, target, dt);
        assert!(
            first > 0.0 && first < target,
            "turn is eased, not a snap: {first}"
        );
        assert!(
            first <= MAX_TURN_RATE * dt + 1e-6,
            "first frame respects the rate cap: {first}"
        );
        let mut frames = 0;
        while wrap_angle_pi(target - yaw).abs() > 1e-6 && frames < 600 {
            yaw = advance_facing(yaw, target, dt);
            frames += 1;
        }
        assert!(
            frames < 60,
            "an about-face lands within a second: {frames} frames"
        );
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
        assert!(
            leant.y < head.y,
            "and drops below standing height: {leant:?}"
        );
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
