use crate::components::AvatarModelLoaded;
use crate::components::{Avatar, RemoteAvatar, RemoteAvatarMotionHint};
use crate::resources::AvatarState;
use crate::systems::locomotion;
use crate::systems::sit_pose;
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
// it read as flight is the forward lean in `update_avatar_model_pose`.
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
/// position is corrected toward the authoritative sim position.
///
/// **Deliberately slow, and paired with [`ONLINE_ERROR_FILTER_RATE`].** It was
/// 9/s — brisk, on the reasoning that prediction integrates the same velocity
/// the sim does, so the residual error is small. What that reasoning misses is
/// that the residual error is *noisy*: the reconcile target is rebuilt every
/// time a snapshot lands, and snapshots do not land on a metronome. A snapshot
/// 40 ms late moves the target by 40 ms of travel — 0.3 m at a walk, 10 m in
/// flight at altitude — and a correction stiff enough to take that out inside a
/// frame or two feeds the whole of that jitter into the picture, twenty times a
/// second. That is the wobble: not divergence, not the link, just arrival times
/// being turned into motion.
///
/// Together the two rates make a second-order response with a damping ratio of
/// ~0.7 and a corner near 0.8 Hz, so snapshot-rate noise is attenuated by three
/// orders of magnitude while a real disagreement is still taken out inside about
/// a second. The client is therefore a little further from the server's opinion
/// at any instant and much smoother, which is the trade this is for.
const ONLINE_RECONCILE_RATE: f32 = 3.5;
/// How fast the correction *follows the error* (1/s) — the second pole.
///
/// Reconciliation acts on a low-passed error rather than on the raw one, so a
/// snapshot that arrives early or late moves the display by a fraction of its
/// lateness instead of by all of it. Set above [`ONLINE_RECONCILE_RATE`] so the
/// pair stays well damped (see there).
const ONLINE_ERROR_FILTER_RATE: f32 = 7.0;
/// Ceiling on the correction speed, as a fraction of the speed being travelled.
///
/// The filter above removes jitter; this bounds whatever is left. A correction
/// is only ever allowed to be a quarter of the avatar's own pace, so it reads as
/// the motion being slightly off rather than as a tug — the one thing a
/// correction must never look like.
const ONLINE_CORRECTION_FRACTION: f32 = 0.25;
/// …and the floor under it (m/s), so a standing avatar still converges: a
/// fraction of zero would leave a stopped avatar parked beside its own position
/// forever.
///
/// **This number decides how fast a real disagreement closes, and nothing
/// else.** Measured 2026-09-11: the clamp does not bind in the jitter regime at
/// any plausible setting — the worst error arrival noise produces is 6 cm at a
/// walk and 22 m at the top of the flight ramp, against a knee (`floor / rate`)
/// of 57 cm and 46 m. The filter above is what removes the wobble; this is only
/// the ceiling on a transient. So it is set by the other end: at 2 m/s a 3 m
/// desync closes in 1.4 s and the worst frame runs at 1.5× walking pace, where
/// 1 m/s took 2.8 s and 3 m/s reached twice walking pace.
const ONLINE_CORRECTION_FLOOR: f32 = 2.0;
/// Cap on how far the authoritative pose is extrapolated while waiting for the
/// next snapshot (see `smooth_online_avatar_display`) — i.e. **how long the
/// client will move on its own prediction with no confirmation**.
///
/// Past the cap the target stops advancing while the display keeps integrating,
/// so reconciliation brakes the display to a halt within `1 / ONLINE_RECONCILE_RATE`
/// seconds and holds it there until the next snapshot lands. That braking is the
/// point — an unconfirmed avatar should stop, not fly on — but the cap decides how
/// often an ordinary hiccup triggers it, and the consequence is proportional to
/// speed: at a walk the avatar loses 20 cm and nobody sees it, at 250 m/s it stops
/// dead and lurches.
///
/// It was 0.15 s, three server ticks, which assumed the only thing between the
/// two ends was a healthy local network. Two ordinary events on a phone exceed it
/// several times a minute and neither is a fault:
///
/// - **The mobile link.** Inter-arrival on cellular routinely spikes to a few
///   hundred milliseconds, and the connection carries chat, prims and voice over
///   one ordered stream, so a single retransmission delays the snapshots queued
///   behind it.
/// - **Our own frame.** `apply_network_snapshot` drains the mailbox once per Bevy
///   frame, so a 200 ms hitch — a tile upload burst during fast flight, exactly
///   when this matters most — ages every queued snapshot by the length of the
///   hitch before any of them is read. The stall then looks like a network fault
///   from inside this function, and isn't one.
///
/// Half a second covers both while still parking a genuinely dead link inside a
/// second. It is deliberately below `ONLINE_SNAP_SECONDS`-worth of travel: a
/// server that has really stopped agreeing with us surfaces as divergence past
/// the cap and then as a snap, rather than being extrapolated away.
const ONLINE_MAX_EXTRAPOLATION: f32 = 0.5;
/// Prediction error (metres) beyond which the display snaps instead of easing —
/// a teleport, a server correction or a resumed connection, not drift. The floor
/// of [`online_snap_error`]; see there for why it cannot be the whole rule.
const ONLINE_SNAP_ERROR: f32 = 8.0;
/// …and the part of that threshold that has to grow with speed: how many seconds
/// of travel count as ordinary prediction lag rather than as a correction.
///
/// Raised from 0.6 s with the reconcile rate: a gentler correction settles at a
/// larger standing error, and a threshold that the ordinary steady state can
/// reach turns every long flight into alternating snaps — which is the judder
/// this whole band of constants exists to keep out of the picture.
const ONLINE_SNAP_SECONDS: f32 = 1.0;

/// The error beyond which the display snaps to authority instead of easing.
///
/// **A fixed distance is a speed-dependent time, and that is what made fast
/// flight stutter.** Reconciliation settles at a standing error of
/// `speed / ONLINE_RECONCILE_RATE` — the display integrates the predicted
/// velocity while the target trails it by the latency the extrapolation cap
/// cannot cover. At a walk that is centimetres and at the 32 m/s flight cap it is
/// ~3.6 m, both comfortably under the 8 m threshold. But flight rides altitude
/// (`motion::fly_altitude_factor`), so a kilometre up the same avatar is doing
/// 250 m/s and the *steady* error is 28 m: the threshold is crossed by nothing
/// having gone wrong, and every frame hard-sets the display to a stale
/// extrapolation instead of easing. Around the crossing point it alternates
/// between the two, which is the judder itself.
///
/// So the threshold is a distance with a *duration* in it. Six tenths of a
/// second of travel is far more than reconciliation's own lag at any speed and
/// far less than any teleport, which moves the avatar to another part of the
/// world; the floor keeps a standing avatar's threshold where it was.
#[must_use]
fn online_snap_error(speed: f32) -> f32 {
    (speed * ONLINE_SNAP_SECONDS).max(ONLINE_SNAP_ERROR)
}

/// The fastest a correction may move the display, at the speed it is travelling.
#[must_use]
fn online_correction_speed(speed: f32) -> f32 {
    (speed * ONLINE_CORRECTION_FRACTION).max(ONLINE_CORRECTION_FLOOR)
}

/// One frame of reconciliation: ease the predicted display toward the
/// authoritative target without letting the target's own arrival noise into the
/// picture.
///
/// `smoothed_error` is this reconciler's memory and belongs to nothing else. The
/// raw error — target minus display — is a jittery signal, because the target is
/// rebuilt whenever a snapshot lands and snapshots arrive when the network says
/// so. Correcting the *filtered* error instead makes the pair a second-order
/// low-pass (see [`ONLINE_RECONCILE_RATE`]), which is what separates "the server
/// disagrees with us" from "that snapshot was 40 ms late".
///
/// Two things deliberately bypass the filter, and they are the two cases where
/// the answer must arrive rather than be eased into:
///
/// - **A snap.** Past [`online_snap_error`] this is a teleport, a server
///   correction or a resumed stream, not drift; easing would slide the avatar
///   across the map instead of putting it where it is.
/// - **A stall.** Once the extrapolation window is spent
///   ([`ONLINE_MAX_EXTRAPOLATION`]) the target stands still on purpose so an
///   unconfirmed avatar brakes to a halt. Filtering and clamping that would make
///   the brake take a second and a half instead of a third of one, and a stall is
///   by definition not jitter.
#[must_use]
fn reconcile_display(
    display: Vec3,
    target: Vec3,
    smoothed_error: &mut Vec3,
    speed: f32,
    stalled: bool,
    dt: f32,
) -> Vec3 {
    let raw = target - display;
    if raw.length() > online_snap_error(speed) {
        *smoothed_error = Vec3::ZERO;
        return target;
    }
    *smoothed_error = if stalled {
        raw
    } else {
        let follow = 1.0 - (-ONLINE_ERROR_FILTER_RATE * dt).exp();
        *smoothed_error + (raw - *smoothed_error) * follow
    };
    let mut step = *smoothed_error * (1.0 - (-ONLINE_RECONCILE_RATE * dt).exp());
    if !stalled {
        step = step.clamp_length_max(online_correction_speed(speed) * dt);
    }
    display + step
}

/// Where the last authoritative pose has got to by now, given how long ago it
/// arrived — the thing reconciliation eases the predicted display toward.
///
/// `AvatarState::position` advances only at the sim's 20 Hz tick, so used raw it
/// is a staircase and the correction drags the display backwards on every frame
/// between two steps. Extrapolating it by its own age compares like with like: the
/// error signal is smooth and reconciliation only corrects real divergence.
///
/// The clamp is where the client stops believing itself
/// ([`ONLINE_MAX_EXTRAPOLATION`]). Past it the target stands still while the
/// display keeps integrating, so the ease brakes the avatar to a halt — deliberate
/// for a link that has actually stopped, and the reason the cap must be generous
/// enough that an ordinary hiccup does not reach it.
///
/// Note it extrapolates by the **predicted** velocity, which is the client's own
/// live input rather than anything the snapshot carried. That is what makes a
/// long window safe: waiting longer means trusting our own keys for longer, not
/// coasting on a stale reading of somebody else's.
#[must_use]
fn extrapolated_target(position: Vec3, predicted_velocity: Vec3, snapshot_age: f32) -> Vec3 {
    position + predicted_velocity * snapshot_age.min(ONLINE_MAX_EXTRAPOLATION)
}

/// Whether a snapshot age has run past the window, i.e. the display is being
/// braked rather than predicted. Counted into
/// [`crate::resources::NetPredictionHealth`], which is the only place this becomes
/// visible to somebody holding the device.
#[must_use]
fn is_stalled(snapshot_age: f32) -> bool {
    snapshot_age >= ONLINE_MAX_EXTRAPOLATION
}
/// Remote rotation should feel a bit snappier than position.
const REMOTE_ROT_SMOOTHING: f32 = 18.0;
/// Horizontal visual speed (m/s) above this plays run animation on remote foxes.
const REMOTE_RUN_SPEED_THRESH: f32 = 0.12;

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

/// How long the arrival takes to play out (seconds).
///
/// A teleport is the one movement in this world with no travel in it: the avatar
/// is at one place on one frame and a hundred kilometres away on the next, and
/// nothing on screen distinguishes that from the map having been redrawn around a
/// figure that never moved. The arrival is what says a body went somewhere.
///
/// Short on purpose. It is played on every jump to a saved place, and anything
/// long enough to be admired is long enough to be waited through the tenth time.
const TELEPORT_ARRIVE_SECS: f32 = 0.45;
/// How thin the avatar is at the start of an arrival, across and along.
///
/// It re-forms out of a vertical column rather than fading in, because the model
/// has no dissolve to fade *with*: the Quaternius characters carry plain opaque
/// materials, and making them transparent for 0.45 s means a second material per
/// slot and a place in the transparent sort — for an effect that is over before
/// it can be looked at. A scale is one transform on a holder that is already
/// being written every frame.
const TELEPORT_ARRIVE_PINCH: f32 = 0.06;
/// How tall the column is at the start, in standing heights. Stated rather than
/// derived from [`TELEPORT_ARRIVE_PINCH`]: preserving volume would want ~280×,
/// which is a light-beam to the stratosphere and not a person.
const TELEPORT_ARRIVE_COLUMN: f32 = 2.4;
/// …and how far it overshoots standing height on the way in, so the body settles
/// rather than stopping dead at 1.0.
const TELEPORT_ARRIVE_STRETCH: f32 = 1.2;

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
/// `Gait::from_modifiers` avoids by letting Run win its tie.
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

/// How fast the *travel* direction turns in flight (rad/s).
///
/// On the ground the wish direction is taken whole, every frame: a walk is
/// footfalls, and a pedestrian who turns has simply changed which way the next
/// step goes. Flight has no footfalls and, at the speeds the altitude ramp
/// reaches, taking the wish whole means the velocity vector rotates ninety
/// degrees between two frames — an instantaneous corner at a few hundred metres
/// per second, which is the other half of what reads as stutter. Easing it makes
/// a turn an arc.
///
/// A quarter turn in about half a second: quick enough to feel like control
/// rather than like a vehicle being steered, slow enough that the corner is gone.
///
/// Deliberately *slower* than the facing turns ([`TURN_SMOOTHING`], capped by
/// [`MAX_TURN_RATE`]), which still tracks the raw wish. So the body comes round
/// first and the velocity follows it through the turn, which is what banking
/// looks like — the other order would have the avatar translating into a
/// direction it is not yet pointing.
const FLIGHT_TURN_RATE: f32 = 3.2;

/// Step the flight travel direction one frame toward `target`, turning at most
/// `max_turn` radians. Both are horizontal unit vectors; a zero `current` (the
/// avatar was at rest) snaps, since there is no heading to arc away from.
#[must_use]
fn advance_move_dir(current: Vec3, target: Vec3, max_turn: f32) -> Vec3 {
    let (Some(from), Some(to)) = (yaw_from_dir(current), yaw_from_dir(target)) else {
        return target;
    };
    let delta = wrap_angle_pi(to - from);
    if delta.abs() <= max_turn {
        return target;
    }
    let yaw = from + delta.signum() * max_turn;
    // Same convention as `wish_dir_camera_relative` / `yaw_from_dir`, so the
    // round trip through an angle is the identity.
    Vec3::new(yaw.sin(), 0.0, yaw.cos())
}

/// Standing height on flat ground: the region tile's top surface (the cuboid's
/// half-extent in y). The avatar models are authored origin-at-feet, so this *is*
/// the resting height — not a half-height offset.
///
/// Public because collision's snap-out backstop has to reconstruct the same
/// ground the movement path stands on (`GROUND_HEIGHT + terrain.ground_at(..)`), and a
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
    /// The seated pose. Not a clip out of the character file — the set has none —
    /// but one this client authors for the rig (`systems::sit_pose`).
    sit_index: AnimationNodeIndex,
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
/// Sitting down is slower than either: it is a whole body lowering itself onto
/// something, and at a gait's fade the avatar snaps into the seat. Paired with
/// [`SEAT_SLIDE_SECS`], which moves it the last half-metre over the same time.
const SIT_FADE: Duration = Duration::from_millis(450);
/// Emotes cut in faster: a wave that eases in over a fifth of a second reads as
/// reluctance.
const EMOTE_FADE: Duration = Duration::from_millis(90);

pub fn spawn_avatar(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut clip_assets: ResMut<Assets<AnimationClip>>,
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
        // `try_insert` for the whole of this spawn: the query includes remote
        // avatars, which `session::teardown` despawns wholesale when a link drops —
        // and nothing orders the two systems, so the entity can already be queued
        // for despawn while it is still visible here.
        commands.entity(entity).try_insert(AvatarModelLoaded);
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
            // …and last the one clip that is not in the file. The character set
            // has no sit, so it is built here for the rig this holder wears
            // (`systems::sit_pose`). Every holder of a mixed avatar gets its own
            // copy, which costs eleven curves and keeps the node indices of all
            // the graphs identical — `update_avatar_animation` reads one
            // `AvatarAnimations` off the avatar entity and drives every holder's
            // player with it.
            clips.push(clip_assets.add(sit_pose::sit_clip(appearance.sex)));
            let (graph, indices) = AnimationGraph::from_clips(clips);
            let anims = AvatarAnimations {
                graph_handle: graphs.add(graph),
                idle_index: indices[0],
                walk_index: indices[1],
                run_index: indices[2],
                // Flight has no clip of its own; the forward lean in
                // `update_avatar_model_pose` is what distinguishes it from
                // standing (`docs/notes/avatar-locomotion.md`).
                fly_index: indices[0],
                emote_indices: indices[3..indices.len() - 1].to_vec(),
                sit_index: indices[indices.len() - 1],
                speed: h.anim_speed,
            };
            if entity_anims.is_none() {
                entity_anims = Some(anims.clone());
            }
            spawn_slot_holder(&mut commands, &asset_server, entity, h.scene, h.mask, anims);
        }
        if let Some(a) = entity_anims {
            commands.entity(entity).try_insert(a);
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
    /// The authored seated pose (`systems::sit_pose`) — the one clip in the set
    /// this client builds rather than loads.
    Sit,
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
    /// One cycle of `node` in seconds, for the two clips whose phase means the
    /// same thing (both `Walk` and `Run` are two footfalls long). `None` for
    /// everything else — idle, flight and the emotes have no stride to carry.
    fn cycle_secs(&self, node: AnimationNodeIndex) -> Option<f32> {
        if node == self.walk_index {
            Some(locomotion::WALK_CLIP_SECS)
        } else if node == self.run_index {
            Some(locomotion::RUN_CLIP_SECS)
        } else {
            None
        }
    }

    fn node(&self, clip: PoseClip) -> AnimationNodeIndex {
        match clip {
            PoseClip::Idle => self.idle_index,
            PoseClip::Walk => self.walk_index,
            PoseClip::Run => self.run_index,
            PoseClip::Fly => self.fly_index,
            PoseClip::Sit => self.sit_index,
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
///
/// # The stride survives a gait change
///
/// `AnimationTransitions::play` starts the incoming clip at seek time **zero**,
/// which for a locomotion clip means "both feet together, about to lead with the
/// left". A walk→run change therefore cross-faded a mid-stride pose into a
/// standing-start one: for the 180 ms of [`GAIT_FADE`] the legs interpolated
/// toward a position they had no business being in, which is the hitch you see
/// the instant Shift goes down or comes up. Nothing is wrong with either clip —
/// they are simply spliced at an arbitrary point in each other's cycle.
///
/// Both clips are exactly two footfalls long ([`locomotion::WALK_CLIP_SECS`] /
/// [`locomotion::RUN_CLIP_SECS`]), so a *normalized* phase is directly
/// transferable: seek the incoming clip to the same fraction of its own cycle
/// and the cross-fade blends left foot into left foot. Idle, flight and the
/// emotes carry no stride and are started from zero as before.
fn apply_pose(
    player: &mut AnimationPlayer,
    transitions: &mut AnimationTransitions,
    anim: &AvatarAnimations,
    pose: Pose,
) {
    let want = anim.node(pose.clip);
    let repeat = !matches!(pose.clip, PoseClip::Emote(_));
    if transitions.get_main_animation() != Some(want) {
        // Where in its cycle the outgoing clip is, as a fraction — read before
        // `play` retires it. `None` whenever either side has no stride.
        let phase = transitions
            .get_main_animation()
            .zip(anim.cycle_secs(want))
            .and_then(|(previous, _)| {
                let secs = anim.cycle_secs(previous)?;
                let active = player.animation(previous)?;
                Some((active.seek_time() / secs).fract())
            });
        let fade = if pose.clip == PoseClip::Sit
            || transitions.get_main_animation() == Some(anim.sit_index)
        {
            // Both ways, so getting up takes as long as sitting down did.
            SIT_FADE
        } else if repeat {
            GAIT_FADE
        } else {
            EMOTE_FADE
        };
        let active = transitions.play(player, want, fade);
        if repeat {
            active.repeat();
        }
        if let Some((phase, secs)) = phase.zip(anim.cycle_secs(want)) {
            // `set_seek_time`, not `seek_to`: the latter records the jump as a
            // range to fire clip events over, and this jump is a splice rather
            // than playback.
            active.set_seek_time(phase * secs);
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

        // Sitting outranks both: it is a state the player is in rather than a
        // motion they asked for, and it is left by walking away, which is what
        // clears it before this is ever reached.
        let pose = match (avatar_state.is_seated(), active_emote) {
            (true, _) => Pose {
                clip: PoseClip::Sit,
                rate: 1.0,
            },
            (false, Some(e)) => Pose {
                clip: PoseClip::Emote(emote_slot(e)),
                rate: 1.0,
            },
            (false, None) => {
                locomotion_pose(avatar_state.ground_speed, avatar_state.is_flying, *running)
            }
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

/// Everything the local avatar's *model* does that its position does not: the
/// forward lean of flight and of a jump, and the slide onto a bench.
///
/// Both live on the model holders rather than on the avatar entity, and for the
/// same reason: the avatar's own transform is its authoritative position and yaw
/// (collision, the follow camera and the replicated facing all read it, and online
/// the sim owns it outright), so pitching or shifting *that* would move all of
/// them. A holder is a child of it, so writing one is a purely visual change.
///
/// **The lean.** The Quaternius "Ultimate Modular" clip set has no flight (or
/// jump) animation and the UAL library can't supply one — different rig, so its
/// clips don't bind (`docs/notes/avatar-locomotion.md`). Flying therefore played
/// *Idle*, i.e. an avatar standing to attention while it crossed the city at
/// 32 m/s. Pitching the model forward is what turns that same pose into a glide;
/// hovering in place stays upright, which is the readable difference between the
/// two. Holders pivot about chest height so the feet swing back instead of the head
/// diving through the ground.
///
/// **The seat.** A bench is a solid prop, so the avatar stops short of the place
/// it has to sit; the gap is carried here as [`AvatarState::seat_shift`], eased
/// over [`SEAT_SLIDE_SECS`] so sitting down is a movement rather than a jump cut.
/// The offset is in world XZ and the holder is under a yawed parent, so it is
/// turned back out of the avatar's frame before it is written.
///
/// **Other players sit here too.** The seat is replicated (protocol 19), so a
/// remote avatar's holders take the same treatment from
/// [`RemoteAvatar::seated`] — but not the lean, which is not on the wire. Each
/// viewer runs the slide itself off its own clock rather than reading a blended
/// offset off the wire twenty times a second.
///
/// Every holder is compared before it is written, so the ordinary standing case
/// costs one comparison per model and marks nothing changed. That replaced an
/// early return, which had to be paired with a "did I write last frame" flag to
/// get the identity back on the frame a pose ended — one more piece of state than
/// the comparison it was avoiding.
pub fn update_avatar_model_pose(
    time: Res<Time>,
    mut avatar_state: ResMut<AvatarState>,
    avatars: Query<Entity, With<Avatar>>,
    mut remotes: Query<&mut RemoteAvatar>,
    mut holders: Query<(&ChildOf, &mut Transform), With<SlotMask>>,
    mut lean: Local<f32>,
    // The arrival: how much of it is *left*, and which teleport it belongs to.
    // Both live here because this is the one system that owns the holder
    // transform — two systems writing it would race for the frame.
    //
    // Counted down rather than up, because a `Local<f32>` starts at zero and zero
    // has to mean *finished*: counting up would have every launch open on the
    // first frame of an arrival nobody asked for.
    mut arrive_left: Local<f32>,
    mut arrive_seq: Local<u32>,
) {
    // Advance the seat slide first, so the offset written below is this frame's.
    // A seat that has finished unwinding is dropped here rather than where the
    // player asked to get up — that is what lets `stand_up` be a one-liner
    // everywhere it is called from.
    let seat_shift = {
        let step = time.delta_secs() / SEAT_SLIDE_SECS;
        let mut done = false;
        if let Some(s) = &mut avatar_state.seated {
            if s.rising {
                s.blend = (s.blend - step).max(0.0);
                done = s.blend <= 0.0;
            } else {
                s.blend = (s.blend + step).min(1.0);
            }
        }
        if done {
            avatar_state.seated = None;
        }
        Quat::from_rotation_y(-avatar_state.facing_yaw) * avatar_state.seat_shift()
    };
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
    // A jump to a saved place restarts the arrival even if one is still playing:
    // the second jump is where the body is now.
    let was_arriving = *arrive_left > 0.0;
    if *arrive_seq != avatar_state.teleport_sequence {
        *arrive_seq = avatar_state.teleport_sequence;
        *arrive_left = TELEPORT_ARRIVE_SECS;
    } else if was_arriving {
        *arrive_left = (*arrive_left - time.delta_secs()).max(0.0);
    }
    let arrived = 1.0 - *arrive_left / TELEPORT_ARRIVE_SECS;

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
    // The same slide, on everyone else's clock. A remote carries the seat it was
    // sent and eases onto it here; a seat that has gone eases back to nothing and
    // the blend simply reaches zero, so there is nothing to retire.
    let step = time.delta_secs() / SEAT_SLIDE_SECS;
    for mut r in remotes.iter_mut() {
        let want = if r.seated.is_some() { 1.0 } else { 0.0 };
        if r.seat_blend != want {
            r.seat_blend = if want > r.seat_blend {
                (r.seat_blend + step).min(1.0)
            } else {
                (r.seat_blend - step).max(0.0)
            };
        }
    }

    let mut local = flight_pose(*lean, arrived);
    local.translation += seat_shift;
    for (parent, mut tf) in holders.iter_mut() {
        let owner = parent.parent();
        let want = if avatars.contains(owner) {
            local
        } else if let Ok(r) = remotes.get(owner) {
            // No lean and no arrival: neither is replicated. Only the seat, turned
            // out of the remote's own facing exactly as the local one is.
            let shift = r.seated.map_or(Vec3::ZERO, |o| {
                let o = o * r.seat_blend;
                Quat::from_rotation_y(-r.net_yaw) * Vec3::new(o.x, 0.0, o.y)
            });
            Transform::from_translation(shift)
        } else {
            continue;
        };
        if *tf != want {
            *tf = want;
        }
    }
}

/// How long the model takes to slide onto a seat, and back off it. Matched to
/// [`SIT_FADE`] so the body finishes arriving as it finishes sitting.
const SEAT_SLIDE_SECS: f32 = 0.45;

/// The holder transform for a given lean and arrival progress.
///
/// The lean is a pitch about the model's X axis taken around a pivot at
/// [`FLIGHT_LEAN_PIVOT_Y`], so the chest stays put and the feet swing back.
///
/// `arrival` runs 0 → 1 over [`TELEPORT_ARRIVE_SECS`] and is a **scale about the
/// feet**: the avatar re-forms out of a thin vertical column, overshoots standing
/// height and settles. Scaling about the feet rather than the origin is the whole
/// difference between materialising *on* the ground and materialising through it —
/// the models are authored origin-at-feet, so the holder's own origin already is
/// the sole of the foot and no pivot correction is needed. At `arrival >= 1` the
/// scale is exactly [`Vec3::ONE`], so an arrival that has finished composes with
/// the lean without leaving a residue.
fn flight_pose(lean: f32, arrival: f32) -> Transform {
    let rotation = Quat::from_rotation_x(lean);
    let pivot = Vec3::Y * FLIGHT_LEAN_PIVOT_Y;
    Transform {
        translation: pivot - rotation * pivot,
        rotation,
        scale: teleport_scale(arrival),
    }
}

/// The arrival's scale at `progress` (0 → 1), [`Vec3::ONE`] once it is done.
///
/// Two eases, because the two axes are telling different halves of the story.
/// **Across** (x and z) the body swells out of a [`TELEPORT_ARRIVE_PINCH`]-thin
/// column on a decelerating curve, so almost all of the widening happens in the
/// first third and the rest is the figure standing there. **Along** (y) it starts
/// stretched by the same amount the column is thin — volume, roughly — passes
/// through [`TELEPORT_ARRIVE_STRETCH`] and settles on 1 with a single soft
/// overshoot. One overshoot reads as a body arriving; a spring with several
/// reads as a bug.
#[must_use]
fn teleport_scale(progress: f32) -> Vec3 {
    if progress >= 1.0 {
        return Vec3::ONE;
    }
    let t = progress.max(0.0);
    // Cubic ease-out: fast at the start, flat at the end.
    let out = 1.0 - (1.0 - t).powi(3);
    let across = TELEPORT_ARRIVE_PINCH + (1.0 - TELEPORT_ARRIVE_PINCH) * out;
    let base = TELEPORT_ARRIVE_COLUMN + (1.0 - TELEPORT_ARRIVE_COLUMN) * out;
    // A half-sine bump on top of that ease, faded in by the ease itself so it
    // cannot lift the opening frame: `along` therefore crosses 1.0 late and comes
    // back down to it, instead of arriving at standing height from one side.
    let bump = (TELEPORT_ARRIVE_STRETCH - 1.0) * (std::f32::consts::PI * t).sin() * out;
    Vec3::new(across, base + bump, across)
}

/// Phase-lock a mixed avatar's per-slot holders: force every holder's active clips
/// to the first holder's seek time, so a head from one style and a body from
/// another (separate skeletons/players that started at slightly different frames)
/// don't drift apart. No-op for a single-holder avatar (a preset or the fox).
pub fn sync_avatar_animations(
    avatars: Query<Entity, Or<(With<Avatar>, With<RemoteAvatar>)>>,
    children: Query<&Children>,
    mut players: Query<&mut AnimationPlayer>,
    // The resolved player entities per avatar. `iter_descendants` walks the
    // avatar's *entire* spawned scene — armature and every bone — and the set
    // it finds only changes on respawn (appearance change) or while a holder's
    // scene is still streaming in, so walking it per avatar per frame paid
    // (remote avatars × skeleton nodes) for an answer that almost never moves.
    // The cache is valid while every cached player is alive and there is one
    // player per holder (holders are the avatar's direct children; each scene
    // binds exactly one `AnimationPlayer` once ready). A respawn despawns the
    // old holders, which kills the cached entities and forces the re-walk; a
    // scene still loading keeps the counts unequal, so it re-walks until every
    // holder's player exists — the same behaviour the uncached walk had.
    mut cache: Local<bevy::ecs::entity::EntityHashMap<Vec<Entity>>>,
    mut seeks: Local<Vec<(AnimationNodeIndex, f32)>>,
) {
    cache.retain(|&e, _| avatars.contains(e));
    for avatar in avatars.iter() {
        let holder_count = children.get(avatar).map(|c| c.len()).unwrap_or(0);
        let valid = cache
            .get(&avatar)
            .is_some_and(|v| v.len() == holder_count && v.iter().all(|&e| players.contains(e)));
        if !valid {
            let found: Vec<Entity> = children
                .iter_descendants(avatar)
                .filter(|&e| players.contains(e))
                .collect();
            cache.insert(avatar, found);
        }
        let holder_players = &cache[&avatar];
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
    // Whether each remote avatar was running last frame, which is what makes the
    // walk/run choice hysteretic instead of flapping at the threshold.
    //
    // `EntityHashMap` rather than `std::collections::HashMap`: keys are entities,
    // and SipHash on an `Entity` is a lot of work to hash a pair of integers.
    mut running: Local<bevy::ecs::entity::EntityHashMap<bool>>,
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
        // Sitting outranks the emote here for the same reason it does locally: it
        // is a state this avatar is in, and the sim drops it the moment its owner
        // asks to move.
        let pose = match (remote.seated.is_some(), active_emote) {
            (true, _) => Pose {
                clip: PoseClip::Sit,
                rate: 1.0,
            },
            (false, Some(e)) => Pose {
                clip: PoseClip::Emote(emote_slot(e)),
                rate: 1.0,
            },
            (false, None) => locomotion_pose(if moving { speed } else { 0.0 }, false, was_running),
        };
        running.insert(avatar_entity, pose.clip == PoseClip::Run);

        for child in children.iter_descendants(avatar_entity) {
            if let Ok((mut player, mut transitions)) = players.get_mut(child) {
                apply_pose(&mut player, &mut transitions, anim, pose);
            }
        }
    }
    // Forget avatars that have gone. Remote avatars are despawned when they leave
    // the area of interest or disconnect, and without this their entries stayed
    // forever — an unbounded leak over a long session on a busy server, and worse
    // than a leak when Bevy recycles an entity index and hands a freshly-arrived
    // avatar the walk/run state of whoever used to hold it.
    running.retain(|entity, _| remote_roots.contains(*entity));
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
    footing: crate::systems::interiors::Footing,
    mut health: ResMut<crate::resources::NetPredictionHealth>,
    // The age this system saw last frame, so the gap a snapshot closed can be read
    // off the frame it resets on. A `Local` and not a field on the resource: it is
    // this system's own bookkeeping and nothing else may write it.
    mut prev_age: Local<f32>,
    // The low-passed prediction error `reconcile_display` corrects on. A `Local`
    // for the same reason: it is the reconciler's memory, and a second writer
    // would be a second opinion about how far off we are.
    mut reconcile_error: Local<Vec3>,
) {
    let dt = time.delta_secs();
    if online.is_some() {
        // A teleport holds authority off for up to `network::TELEPORT_HOLD_S`
        // while the sim's echo comes back, so the age climbs for a reason that has
        // nothing to do with the link. Measuring those frames would report a
        // two-second network gap every time somebody taps a saved place, which is
        // the fastest way to make an instrument not worth reading.
        //
        // **Device tracking sets the same hold about once a second**, because a
        // GNSS fix is placed through `NetMessage::Teleport` too
        // (`driver::place_avatar`). At a walking pace the next snapshot is well
        // inside `TELEPORT_ECHO_M` and clears it within a frame, so almost
        // nothing is skipped; in a vehicle the hold lasts a round trip and this
        // stops counting for perhaps a tenth of the frames. That is the right
        // answer rather than a hole in the instrument — while a fix owns the
        // position it is being *written*, not predicted, so those frames have no
        // prediction health to report. Worth knowing before reading a `parked`
        // share taken with the locate button lit or while driving.
        if avatar_state.teleport_hold.is_none() {
            // A snapshot landed since this system last ran: `apply_network_snapshot`
            // is ordered ahead of us and zeroes the age, and nothing else does. So
            // last frame's age is the gap that snapshot closed.
            if avatar_state.snapshot_age == 0.0 && *prev_age > 0.0 {
                health.last_gap_s = *prev_age;
                health.worst_gap_s = health.worst_gap_s.max(*prev_age);
                health.snapshots += 1;
            }
            health.frames += 1;
            if is_stalled(avatar_state.snapshot_age + dt) {
                health.stalled_frames += 1;
            }
        }
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
        avatar_state.snapshot_age += dt;
        let mut target = extrapolated_target(
            avatar_state.position,
            avatar_state.predicted_velocity,
            avatar_state.snapshot_age,
        );
        let mut display = avatar_state.display_position + avatar_state.predicted_velocity * dt;
        // The sim has no terrain and never will — it holds no map data at all, and
        // the map streams only on the client. So the ground is added *here*, on the
        // display side: the wire, the prediction and the reconciliation all stay on
        // the flat plane the sim actually simulates, and only what you see stands on
        // the measured one. Last frame's lift comes off first for exactly that
        // reason — smoothing a lifted position toward a flat target would drag the
        // avatar back into the hillside every frame, and the two would fight for as
        // long as you stood on a slope.
        // Both lifts come off, for the reason the comment above gives: what is
        // smoothed toward a flat target has to be flat itself, or the avatar is
        // dragged back down through the storey it is standing on every frame.
        display.y -= avatar_state.terrain_lift + avatar_state.floor_lift;
        // Match the sim's ground clamp (`state::step`, y >= 0) so a fly-down
        // prediction doesn't dip through the floor and bounce back.
        if display.y < 0.0 {
            display.y = 0.0;
        }
        if target.y < 0.0 {
            target.y = 0.0;
        }
        // Measured against the speed actually being predicted, not the ramped
        // `ground_speed`: the altitude factor is applied at integration, so at
        // 2 km up those two differ by a factor of sixteen.
        let flat = reconcile_display(
            display,
            target,
            &mut reconcile_error,
            avatar_state.predicted_velocity.length(),
            is_stalled(avatar_state.snapshot_age),
            dt,
        );
        avatar_state.terrain_lift = terrain.ground_at(flat.x, flat.z);
        // The storey, on top of the ground. Sampled from `flat` — the reconciled
        // sim position, which is the frame `interiors` builds its rooms in and the
        // frame `send_network_intent` resolves collision in — so the floor the
        // avatar stands on is the floor it can see and the wall it just walked
        // through.
        let floor = footing.step(&mut avatar_state, Vec2::new(flat.x, flat.z), dt);
        avatar_state.display_position = flat + Vec3::Y * (avatar_state.terrain_lift + floor);
        // What the follow camera leads by. The predicted velocity is the honest
        // answer online: it is precisely the rate `display_position` advances at
        // between snapshots, which is what the rig is trying to keep up with.
        avatar_state.velocity = avatar_state.predicted_velocity;
        *prev_age = avatar_state.snapshot_age;
        if let Ok(mut tf) = avatar_query.single_mut() {
            tf.translation = avatar_state.display_position;
        }
    } else {
        avatar_state.display_position = avatar_state.position;
        *prev_age = 0.0;
        // Offline the transform *is* the position, so there is no error to carry.
        // Zeroed rather than left standing: a session can be entered and left
        // while the app runs, and the first online frame must not correct for a
        // disagreement that belongs to a connection which has already gone.
        *reconcile_error = Vec3::ZERO;
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
    footing: crate::systems::interiors::Footing,
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
        // Same reasoning for the velocity the follow rig leads by: returning to
        // Follow snaps the camera onto its target, and a lead left over from
        // whatever the avatar was doing when the mode changed would place that
        // snap tens of metres off (`free_camera::FOLLOW_LEAD_MAX`).
        avatar_state.velocity = Vec3::ZERO;
        // …and stand it up. The seat is a Follow-mode arrangement: the prompt that
        // offers it only appears there, and a free camera flying away from a
        // seated avatar leaves nothing to get it off the bench.
        avatar_state.stand_up();
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
    // Gait modifiers. Shift runs, Alt strolls, neither walks — see
    // `systems::locomotion`. Shift is free in Follow (the camera's boost is a Fly
    // binding) and Alt is unused anywhere in this client. Note the *unmodified*
    // gait is the walk: a modifier is for leaving the everyday pace, not for
    // reaching it.
    let run =
        keyboard_input.pressed(KeyCode::ShiftLeft) || keyboard_input.pressed(KeyCode::ShiftRight);
    let stroll =
        keyboard_input.pressed(KeyCode::AltLeft) || keyboard_input.pressed(KeyCode::AltRight);
    avatar_state.gait = locomotion::Gait::from_modifiers(run, stroll);

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

    // Sitting is left by doing anything else. Any of the three — a step, a jump,
    // a take-off — stands the avatar up, and `stand_up` takes effect this frame,
    // so the input that ended the sit is also acted on rather than swallowed.
    if avatar_state.is_seated() && (has_input || avatar_state.wants_jump || avatar_state.is_flying)
    {
        avatar_state.stand_up();
    }
    if avatar_state.is_seated() {
        // Held still and turned to face out of the bench. Everything below is
        // about going somewhere, and this avatar is not.
        avatar_state.is_walking = false;
        avatar_state.ground_speed = 0.0;
        avatar_state.move_dir = Vec3::ZERO;
        avatar_state.wants_jump = false;
        if let Some(seat) = avatar_state.seated {
            avatar_state.facing_yaw = advance_facing(avatar_state.facing_yaw, seat.yaw, delta_time);
        }
        let rotation = Quat::from_rotation_y(avatar_state.facing_yaw);
        if transform.rotation != rotation {
            transform.rotation = rotation;
        }
        return;
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
        // On the ground the wish is taken whole (movement is immediate — see
        // above). In flight the heading arcs toward it instead, unless there is
        // no heading yet: a take-off from rest, or a stop, has nothing to turn
        // away from and would otherwise curve out of a standstill.
        avatar_state.move_dir =
            if avatar_state.is_flying && avatar_state.ground_speed > locomotion::MOVING_EPS {
                advance_move_dir(avatar_state.move_dir, wish, FLIGHT_TURN_RATE * delta_time)
            } else {
                wish
            };
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
    // Where this frame's integration started, so the velocity the follow camera
    // leads by can be *measured* rather than restated. Collision steering, the
    // ground clamp and the altitude factor all move the avatar by less (or by
    // something other) than the nominal speed, and a camera led by a velocity the
    // avatar did not achieve overshoots into every wall it slides along.
    let before = transform.translation;

    // Flight rides altitude, at integration — the same
    // `motion::fly_altitude_factor` the sim applies in `step` and prediction
    // applies in `send_network_intent`, so the launch mode cannot change how fast
    // a height flies. `ground_speed` itself stays the base rate: it feeds the
    // animation and the intent's wish encoding, both of which saturate.
    let lift = if avatar_state.is_flying {
        motion::fly_altitude_factor(transform.translation.y)
    } else {
        1.0
    };
    if avatar_state.ground_speed > 1e-4 {
        let delta = avatar_state.move_dir * avatar_state.ground_speed * lift * delta_time;
        // Collide against buildings while walking (anything off the ground passes
        // over roofs — the footprints are 2D, so they'd otherwise divert a flight
        // or a fall around a building it clears) when the buildings layer is on.
        // Footprints live in the anchored world-XZ frame, so resolve the move in
        // absolute metres (cell + local).
        // Last frame's storey is part of the ground here, exactly as the terrain
        // is (`collision::eject_avatar_from_buildings` says why): offline this
        // transform *is* the avatar's state, so standing on a first floor puts
        // `y` 3.2 m up and without the lift every avatar upstairs reads as
        // airborne — and an airborne avatar skips collision entirely, so the
        // walls of every upper storey stop being walls.
        let airborne = transform.translation.y
            > GROUND_HEIGHT
                + terrain.ground_at(transform.translation.x, transform.translation.z)
                + avatar_state.floor_lift
                + AIRBORNE_EPS;
        // Underground, the surface world is not what is around you: the
        // building over a tunnel is seven metres up and its footprint is not a
        // wall down here, while the bore's own wall is the only thing that is
        // (`systems::tunnels`). So the two collision worlds are exclusive, and
        // which one applies is decided by the floor the avatar is standing on.
        let in_bore = crate::systems::tunnels::inside(avatar_state.floor_lift);
        if !airborne && (in_bore || (layers.buildings_enabled && !colliders.is_empty())) {
            use crate::grid::WORLD_CELL_EDGE;
            let base = Vec2::new(
                grid_cell.x as f32 * WORLD_CELL_EDGE,
                grid_cell.z as f32 * WORLD_CELL_EDGE,
            );
            let from = base + Vec2::new(transform.translation.x, transform.translation.z);
            let to = from + Vec2::new(delta.x, delta.z);
            let resolved = if in_bore {
                colliders.slide_in_bore(from, to)
            } else {
                colliders.slide(from, to)
            };
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
    // The storey the avatar is standing on, if it is indoors. Same shape as the
    // terrain above it — the ground level is whatever is under your feet, and a
    // first-floor slab is as much "the ground" as a hillside is. Sampled in the
    // frame `interiors` built its rooms in, which is this transform's.
    let floor = footing.step(
        &mut avatar_state,
        Vec2::new(transform.translation.x, transform.translation.z),
        delta_time,
    );
    let ground_y =
        GROUND_HEIGHT + terrain.ground_at(transform.translation.x, transform.translation.z) + floor;
    if avatar_state.is_flying {
        // Ramped, not switched — `motion::step_fly_vertical`, the same curve the
        // sim runs and `send_network_intent` predicts. Descent stops asking once
        // the ground is reached, so the thrust unwinds instead of being held
        // against the floor.
        let target = if fly_up {
            motion::FLY_VERTICAL_SPEED
        } else if fly_down && transform.translation.y > ground_y {
            -motion::FLY_VERTICAL_SPEED
        } else {
            0.0
        };
        avatar_state.fly_vertical =
            motion::step_fly_vertical(avatar_state.fly_vertical, target, delta_time);
        transform.translation.y += avatar_state.fly_vertical * lift * delta_time;
        transform.translation.y = transform.translation.y.max(ground_y);
        avatar_state.vertical_velocity = 0.0;
        avatar_state.grounded = false;
    } else {
        avatar_state.fly_vertical = 0.0;
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
    avatar_state.velocity = (transform.translation - before) / delta_time.max(1e-4);
}

/// How many of the avatar's meshes exist, and how many the view drew.
///
/// A `SystemParam` because the answer takes three queries and its first reader,
/// `shot_harness::drive_capture`, was already at Bevy's 16-parameter ceiling. The
/// three belong together anyway: they are one question asked in three steps, since
/// the model meshes carry no marker of their own. They are glTF nodes instantiated
/// under a holder, so the only thing identifying them as the avatar's is that the
/// avatar is their ancestor.
#[derive(bevy::ecs::system::SystemParam)]
pub struct AvatarReadout<'w, 's> {
    avatars: Query<'w, 's, Entity, With<Avatar>>,
    children: Query<'w, 's, &'static Children>,
    meshes: Query<
        'w,
        's,
        &'static bevy::camera::visibility::ViewVisibility,
        With<bevy::render::mesh::Mesh3d>,
    >,
}

impl AvatarReadout<'_, '_> {
    /// `(meshes under the local avatar, how many the view rendered)`.
    ///
    /// Zero for both when there is no avatar entity at all, which is the honest
    /// answer for a run that has none rather than a reason to fail.
    pub(crate) fn counts(&self) -> (usize, usize) {
        let Ok(avatar) = self.avatars.single() else {
            return (0, 0);
        };
        let (mut total, mut drawn) = (0, 0);
        for entity in self.children.iter_descendants(avatar) {
            if let Ok(visibility) = self.meshes.get(entity) {
                total += 1;
                if visibility.get() {
                    drawn += 1;
                }
            }
        }
        (total, drawn)
    }
}

/// Log the spawned-vs-drawn split whenever it changes.
///
/// `ShotMetrics` has carried `avatar_meshes`/`avatar_rendered` since 2026-08-14,
/// and they answer the invisible-avatar question exactly — `0/0` is "never
/// spawned", `n/0` is "spawned but not drawn". They were unreachable on the one
/// platform that needed them: a scripted capture is driven by CLI flags, and
/// Android has no CLI at all (`lib.rs::parse_cli` hands it a fixed `--offline`,
/// because the zygote's `argv` belongs to `app_process`). So the phone could
/// report `avatar: model ready nodes=78 hidden=0` — every mesh present and
/// unmasked — and still leave "is any of it on screen?" unanswerable without
/// attaching a debugger to a device nobody can attach a debugger to.
///
/// On change rather than per frame, which makes it an instrument instead of
/// chatter: it fires a handful of times while the model streams in and then goes
/// quiet, so a steady state costs one comparison and one log line total. That also
/// makes the *transition* legible — `10/10` dropping to `10/0` names the frame the
/// avatar stopped being drawn, which a periodic sample would blur.
///
/// Deliberately not gated to Android or to a dev build. It is three queries and a
/// tuple compare, the desktop log is where the phone's behaviour gets compared
/// against a known-good one, and a diagnostic that only exists on the platform you
/// cannot debug is a diagnostic nobody has ever seen working.
pub fn log_avatar_draw_counts(
    readout: AvatarReadout,
    camera_state: Res<crate::resources::CameraState>,
    // The camera is big_space's floating origin, so an entity's `GlobalTransform`
    // *is* its offset from the camera in metres — the camera↔avatar distance needs
    // no cell arithmetic, just the length of this.
    avatars: Query<&GlobalTransform, With<Avatar>>,
    mut last: Local<Option<(usize, usize, &'static str, i64)>>,
) {
    let (meshes, rendered) = readout.counts();
    // Skip the initial (0, 0): the avatar entity exists from `Startup` but its
    // glTF is asynchronous, so every run would open with a "no meshes" line that
    // is merely early rather than wrong.
    if (meshes, rendered) == (0, 0) && last.is_none() {
        return;
    }
    let mode = match camera_state.mode {
        crate::resources::CameraMode::Avatar => "follow",
        crate::resources::CameraMode::Free => "fly",
        crate::resources::CameraMode::Orbit => "earth",
    };
    // Rounded to a decade of metres so a walking avatar doesn't re-log every frame
    // while a camera stranded a kilometre away still shows up as one line.
    let distance = avatars
        .single()
        .map(|t| t.translation().length())
        .unwrap_or(f32::NAN);
    let banded = if distance.is_finite() {
        (distance as i64 / 10) * 10
    } else {
        -1
    };
    let key = (meshes, rendered, mode, banded);
    if *last == Some(key) {
        return;
    }
    *last = Some(key);
    // `rendered` counts entities that passed view-visibility, which is a CPU-side
    // frustum test — it says "submitted", not "you can see it". An avatar the
    // camera is a kilometre above passes that test and is still sub-pixel, so the
    // distance is logged beside the counts: `10/10` at `distance_m = 1478` is the
    // shape of "drawn, but not where anyone is looking".
    tracing::info!(
        meshes,
        rendered,
        mode,
        distance_m = distance as i64,
        "avatar: meshes drawn (spawned/rendered; distance is camera→avatar)"
    );
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

    /// The flight heading arcs toward the wish instead of snapping to it, and
    /// always arrives — a turn that asymptotes never finishes, and a `move_dir`
    /// that keeps changing by a hair re-sends an intent every frame.
    #[test]
    fn the_flight_heading_arcs_and_arrives() {
        let dt = 1.0 / 60.0;
        let step = FLIGHT_TURN_RATE * dt;
        let north = Vec3::new(0.0, 0.0, 1.0);
        let east = Vec3::new(1.0, 0.0, 0.0);

        // One frame does not get you there…
        let first = advance_move_dir(north, east, step);
        assert!(
            first.angle_between(north) > 0.0 && first.angle_between(east) > 1e-3,
            "a quarter turn cannot happen in one frame: {first:?}"
        );

        // …but it converges, exactly, in about the time the constant promises.
        let mut dir = north;
        let mut frames = 0;
        while dir.angle_between(east) > 1e-4 && frames < 600 {
            dir = advance_move_dir(dir, east, step);
            frames += 1;
        }
        assert_eq!(dir, east, "settles exactly on the wish");
        let secs = frames as f32 * dt;
        assert!(
            (0.3..=0.8).contains(&secs),
            "a quarter turn should take about half a second: {secs}"
        );

        // The direction stays a unit horizontal vector throughout — it becomes a
        // velocity by being scaled, so any length drift is a speed error.
        let mid = advance_move_dir(north, east, step);
        assert!((mid.length() - 1.0).abs() < 1e-5, "not unit: {mid:?}");
        assert_eq!(mid.y, 0.0, "flight steers horizontally: {mid:?}");

        // Turning the short way round: a wish just past behind must not sweep
        // the long way about.
        let behind_left = Vec3::new(-0.2, 0.0, -1.0).normalize();
        let turned = advance_move_dir(north, behind_left, step);
        assert!(turned.x < 0.0, "took the long way round: {turned:?}");

        // No heading yet (at rest, or an unusable target) snaps rather than arcs.
        assert_eq!(advance_move_dir(Vec3::ZERO, east, step), east);
        assert_eq!(advance_move_dir(north, Vec3::ZERO, step), Vec3::ZERO);
    }

    /// One frame of the online display loop, as `smooth_online_avatar_display`
    /// runs it: integrate the prediction, then reconcile toward the last
    /// authoritative pose extrapolated by its own age.
    fn step_display(
        display: Vec3,
        authority: Vec3,
        velocity: Vec3,
        age: f32,
        error: &mut Vec3,
        dt: f32,
    ) -> Vec3 {
        let target = extrapolated_target(authority, velocity, age);
        reconcile_display(
            display + velocity * dt,
            target,
            error,
            velocity.length(),
            is_stalled(age),
            dt,
        )
    }

    /// **Snapshots arrive when the network says so, and that must not become
    /// motion.** This is the wobble the reconciler is tuned against, and it has
    /// nothing to do with the server disagreeing: here the sim integrates exactly
    /// the velocity the client predicts, and the only thing moving the reconcile
    /// target about is arrival times drifting either side of the tick.
    ///
    /// A stiff correction turns each late arrival into a shove — at 9/s a 40 ms
    /// gap in the news was worth a third of the walking speed, twenty times a
    /// second. The assertion is on the *displacement per frame*, because that is
    /// what a person sees.
    /// Run a steady journey at `speed` against a sim that agrees with us
    /// completely, and report the worst a single frame's travel deviated from
    /// the nominal — plus the largest error the arrival noise ever opened up.
    ///
    /// The two clocks are kept apart on purpose, because that is where the noise
    /// comes from and it is easy to model it away by accident: the sim's
    /// position advances on its own 20 Hz tick, and the news of a given tick
    /// arrives a latency *plus a varying delay* later. Tie the two together —
    /// derive the authority from the arrival time — and the extrapolation
    /// cancels the jitter exactly, the test passes at any tuning, and it is
    /// measuring nothing.
    fn journey(
        speed: f32,
        reconcile: impl Fn(Vec3, Vec3, &mut Vec3, f32, bool, f32) -> Vec3,
    ) -> (f32, f32) {
        let dt = 1.0 / 60.0;
        let tick = 1.0 / 20.0;
        let v = Vec3::new(0.0, 0.0, -speed);
        let latency = 0.08;
        // What the link adds on top of the latency, per snapshot, cycling.
        let jitter = [0.0, 0.04, -0.01, 0.03, 0.0, 0.05, -0.02, 0.02];

        let mut t = 0.0f32;
        let mut sent = 0usize;
        let mut authority = Vec3::ZERO;
        let mut age = 0.0f32;
        let mut display = Vec3::ZERO;
        let mut error = Vec3::ZERO;
        let (mut worst_step, mut worst_error) = (0.0f32, 0.0f32);
        for frame in 0..1800 {
            t += dt;
            age += dt;
            loop {
                let at = sent as f32 * tick;
                if at + latency + jitter[sent % jitter.len()] > t {
                    break;
                }
                authority = v * at;
                age = t - (at + latency + jitter[sent % jitter.len()]);
                sent += 1;
            }
            let target = extrapolated_target(authority, v, age);
            let before = display;
            display = reconcile(
                display + v * dt,
                target,
                &mut error,
                speed,
                is_stalled(age),
                dt,
            );
            // Skip the opening seconds, where the display is still catching up to
            // a lag it started with none of.
            if frame > 300 {
                worst_step =
                    worst_step.max(((display - before).length() - speed * dt).abs() / (speed * dt));
                worst_error = worst_error.max((target - display).length());
            }
        }
        (worst_step, worst_error)
    }

    #[test]
    fn arrival_jitter_does_not_reach_the_picture() {
        for speed in [
            crate::systems::locomotion::WALK_SPEED,
            crate::systems::locomotion::FLY_SPEED,
            // Flight at altitude, where the same lateness is worth ten metres.
            crate::systems::locomotion::FLY_SPEED * motion::FLY_ALT_MAX_FACTOR,
        ] {
            let (worst, error) = journey(speed, reconcile_display);
            assert!(
                worst < 0.05,
                "at {speed} m/s a frame's travel wobbled by {:.1}% of itself",
                worst * 100.0
            );

            // …and the *filter* is what achieved that, not the correction cap.
            // The cap is a ceiling on a transient; if it were binding on ordinary
            // arrival noise it would be shaping the everyday motion, which is the
            // job this design gives to the filter alone. Measured 2026-09-11:
            // the noise opens 6 cm at a walk against a 57 cm knee.
            let knee = online_correction_speed(speed) / ONLINE_RECONCILE_RATE;
            assert!(
                error < knee,
                "at {speed} m/s arrival noise reaches {error} m, past the {knee} m \
                 where the correction cap starts to bite"
            );
        }
    }

    /// The same journey under the old single-pole correction, to pin *why* the
    /// rates changed rather than leaving them as two numbers somebody will round
    /// off. Measured 2026-09-11: 30.5 % against 2.3 %.
    #[test]
    fn the_old_stiff_correction_is_what_the_wobble_was() {
        /// What `smooth_online_avatar_display` did before 2026-09-11: one pole at
        /// 9/s, straight onto the raw error.
        fn old(
            display: Vec3,
            target: Vec3,
            _error: &mut Vec3,
            speed: f32,
            _stalled: bool,
            dt: f32,
        ) -> Vec3 {
            if display.distance(target) > online_snap_error(speed) {
                target
            } else {
                display.lerp(target, 1.0 - (-9.0f32 * dt).exp())
            }
        }
        let (was, _) = journey(crate::systems::locomotion::WALK_SPEED, old);
        let (now, _) = journey(crate::systems::locomotion::WALK_SPEED, reconcile_display);
        assert!(
            was > 0.2,
            "the 9/s correction used to pass {:.1}% of each frame's travel as wobble",
            was * 100.0
        );
        assert!(now * 5.0 < was, "now {now}, was {was}");
    }

    /// …and it is still reconciliation: a server that genuinely disagrees is
    /// agreed with, inside about a second, without ever tugging.
    #[test]
    fn a_real_disagreement_is_corrected_without_a_tug() {
        let dt = 1.0 / 60.0;
        let speed = crate::systems::locomotion::WALK_SPEED;
        let v = Vec3::new(0.0, 0.0, -speed);
        // Three metres off, and under the snap threshold, so this is drift to be
        // eased out rather than a teleport to be arrived at.
        let offset = Vec3::new(3.0, 0.0, 0.0);
        assert!(offset.length() < online_snap_error(speed));

        let mut display = offset;
        let mut error = Vec3::ZERO;
        let mut worst_step = 0.0f32;
        let mut t = 0.0f32;
        for _ in 0..90 {
            t += dt;
            let before = display;
            // Snapshots on time, so the only error is the one we started with.
            display = step_display(display, v * t, v, 0.0, &mut error, dt);
            worst_step = worst_step.max((display - before).length() / dt);
        }
        let left = (display - v * t).length();
        assert!(left < 0.3, "1.5 s later the display is still {left} m out");
        assert!(
            worst_step <= speed + online_correction_speed(speed) + 1e-3,
            "the correction added {worst_step} m/s to a {speed} m/s walk"
        );
    }

    /// A teleport still arrives rather than being eased into — and takes the
    /// filter's memory with it, or the next frames would correct for a
    /// disagreement that was resolved by moving.
    #[test]
    fn a_teleport_snaps_and_forgets() {
        let speed = crate::systems::locomotion::WALK_SPEED;
        let mut error = Vec3::new(0.4, 0.0, 0.0);
        let here = Vec3::ZERO;
        let there = Vec3::new(0.0, 0.0, -2_000.0);
        let landed = reconcile_display(here, there, &mut error, speed, false, 1.0 / 60.0);
        assert_eq!(landed, there);
        assert_eq!(error, Vec3::ZERO);
    }

    /// The snap threshold is a *duration* of travel, not a fixed distance.
    ///
    /// This is the property fast flight stutters without: reconciliation's own
    /// standing error is `speed / ONLINE_RECONCILE_RATE`, so a threshold that does
    /// not grow with speed is eventually crossed by nothing having gone wrong, and
    /// the display hard-sets to a stale extrapolation every frame instead of
    /// easing.
    #[test]
    fn the_snap_threshold_outruns_reconciliations_own_error() {
        // Standing and walking keep the old threshold.
        assert_eq!(online_snap_error(0.0), ONLINE_SNAP_ERROR);
        assert_eq!(
            online_snap_error(crate::systems::locomotion::WALK_SPEED),
            ONLINE_SNAP_ERROR
        );

        // At every speed flight can reach — the cap, and the cap scaled by the
        // altitude ramp — the threshold stays clear of the error reconciliation
        // settles at, with room for the latency the extrapolation cap misses.
        for speed in [
            crate::systems::locomotion::FLY_SPEED,
            crate::systems::locomotion::FLY_SPEED * 4.0,
            crate::systems::locomotion::FLY_SPEED * motion::FLY_ALT_MAX_FACTOR,
        ] {
            let settled = speed / ONLINE_RECONCILE_RATE;
            assert!(
                online_snap_error(speed) > settled * 2.0,
                "at {speed} m/s the threshold is {} against a standing error of {settled}",
                online_snap_error(speed)
            );
        }

        // …and never so generous that a teleport eases instead of snapping. The
        // shortest jump anyone makes is to another part of the same city.
        assert!(
            online_snap_error(crate::systems::locomotion::FLY_SPEED * motion::FLY_ALT_MAX_FACTOR)
                < 1_000.0
        );
    }

    /// The prediction keeps moving through the two ordinary events that used to
    /// park it, and still stops for a link that has genuinely gone.
    ///
    /// Past [`ONLINE_MAX_EXTRAPOLATION`] the reconcile target stands still while
    /// the display keeps integrating, so the ease brakes the avatar to a halt —
    /// which at a walk costs 20 cm and nobody sees, and in flight at altitude is a
    /// dead stop and a lurch. The window therefore has to outlast a hiccup and
    /// nothing more.
    #[test]
    fn the_prediction_rides_out_a_late_snapshot() {
        // The two events that reach this code as a late snapshot and are not
        // faults. Both are why 0.15 s (three server ticks) was too tight.
        let server_tick = 1.0 / 20.0;
        assert!(
            !is_stalled(server_tick * 4.0),
            "four ticks of jitter on a mobile link is ordinary, not a stall"
        );
        assert!(
            !is_stalled(0.2),
            "a 200 ms frame hitch delays our own mailbox drain by its own length"
        );

        // …and it still parks a link that has actually stopped. The brake takes
        // about `1 / ONLINE_RECONCILE_RATE` on top, so this is what decides
        // whether a dropped connection reads as stopping or as flying on.
        assert!(is_stalled(1.0), "a whole second with nothing is a stall");
        assert!(
            ONLINE_MAX_EXTRAPOLATION + 1.0 / ONLINE_RECONCILE_RATE < 1.0,
            "an unconfirmed avatar should be stopped inside a second"
        );

        // The window must stay under the snap's own duration, or a server that has
        // really stopped agreeing with us is extrapolated away instead of
        // surfacing as divergence and then as a snap.
        assert!(ONLINE_MAX_EXTRAPOLATION < ONLINE_SNAP_SECONDS);

        // What the window buys, at the speed it matters: the target keeps pace
        // with the prediction through a late snapshot instead of standing still.
        let v = Vec3::new(0.0, 0.0, -crate::systems::locomotion::FLY_SPEED * 8.0);
        let here = Vec3::ZERO;
        for age in [server_tick, 0.2, 0.4] {
            let moved = extrapolated_target(here, v, age).distance(here);
            let expected = v.length() * age;
            assert!(
                (moved - expected).abs() < 1e-3,
                "at {age}s the target should have travelled {expected} m, not {moved}"
            );
        }
        // Past the window it stops, which is the brake being applied on purpose.
        let capped = extrapolated_target(here, v, 5.0).distance(here);
        assert!(
            (capped - v.length() * ONLINE_MAX_EXTRAPOLATION).abs() < 1e-3,
            "the window is not being clamped: {capped}"
        );

        // A standing avatar has no extrapolation to do at any age, so a bad link
        // can never move one.
        assert_eq!(extrapolated_target(here, Vec3::ZERO, 9.0), here);
    }

    /// The arrival grows out of a thin column, overshoots once, and finishes at
    /// exactly the identity — a residue would leave every teleported avatar
    /// slightly the wrong size for the rest of the run.
    #[test]
    fn the_teleport_arrival_settles_on_standing_height() {
        assert_eq!(teleport_scale(1.0), Vec3::ONE);
        assert_eq!(teleport_scale(2.0), Vec3::ONE, "and stays there");

        let start = teleport_scale(0.0);
        assert!(start.x < 0.2 && start.z < 0.2, "starts thin: {start:?}");
        assert!(start.y > 1.5, "and tall: {start:?}");
        assert_eq!(start.x, start.z, "round, not flattened: {start:?}");

        // Monotone widening, and a single overshoot in height — several would
        // read as a wobble rather than as a body settling.
        let steps: Vec<Vec3> = (0..=40).map(|i| teleport_scale(i as f32 / 40.0)).collect();
        for w in steps.windows(2) {
            assert!(w[1].x >= w[0].x - 1e-5, "width went backwards: {w:?}");
        }
        // Height comes *down* to standing from the column, overshooting it once
        // on the way — so it approaches 1.0 from above and never dips under, which
        // is a body settling rather than a knee buckling.
        assert!(
            steps.iter().any(|s| s.y > 1.0 + 0.05),
            "the overshoot is too small to see"
        );
        assert!(
            steps.iter().all(|s| s.y >= 1.0 - 1e-4),
            "the arrival must not squash below standing height"
        );
        assert!(
            steps.iter().all(|s| s.y < 3.0 && s.x > 0.0),
            "nothing absurd along the way"
        );
        // The last stretch is monotone down onto 1.0: no second bounce.
        for w in steps[steps.len() * 3 / 4..].windows(2) {
            assert!(w[1].y <= w[0].y + 1e-5, "bounced on the way in: {w:?}");
        }
    }

    /// **The arrival is applied to the real holder, and taken back off it.**
    ///
    /// The pure curve is checked above; this checks the system that carries it,
    /// because both of the defects this feature actually had were in the
    /// *plumbing* rather than in the arithmetic, and neither could fail a test on
    /// `teleport_scale`:
    ///
    /// 1. `Local<f32>` starts at zero. With the timer counting *up*, zero meant
    ///    "just started" and every launch opened on the first frame of an arrival
    ///    nobody asked for.
    /// 2. The pose is written only while something is happening, so the frame the
    ///    arrival *ends* has to write the resting value. Without that the holder
    ///    keeps the last non-identity scale for the rest of the run — a teleported
    ///    avatar left permanently a few per cent thin, which nothing would report.
    #[test]
    fn a_teleport_scales_the_holder_and_then_restores_it() {
        // No `TimePlugin`: it drives `Time` off the wall clock, which between two
        // `app.update()` calls in a test is microseconds — the arrival would still
        // be on its first frame at the end of the test and every assertion below
        // would pass or fail for the wrong reason. An inserted `Time` is nobody
        // else's to overwrite, so the test owns the clock outright.
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .init_resource::<AvatarState>()
            .add_systems(Update, update_avatar_model_pose);
        let holder = app
            .world_mut()
            .spawn((SlotMask::ALL, Transform::IDENTITY))
            .id();
        app.world_mut().spawn(Avatar).add_child(holder);

        let scale_of = |app: &App| {
            app.world()
                .get::<Transform>(holder)
                .expect("the holder spawned above")
                .scale
        };
        let step = |app: &mut App, secs: f32| {
            app.world_mut()
                .resource_mut::<Time<()>>()
                .advance_by(std::time::Duration::from_secs_f32(secs));
            app.update();
        };

        // Startup: nobody has teleported, so the holder must be left alone.
        app.update();
        assert_eq!(
            scale_of(&app),
            Vec3::ONE,
            "an avatar that has not teleported must not play an arrival"
        );

        // A teleport lands: the body is pinched and tall on the next frame.
        app.world_mut()
            .resource_mut::<AvatarState>()
            .teleport_sequence = 1;
        step(&mut app, 1.0 / 60.0);
        let arriving = scale_of(&app);
        assert!(
            arriving.x < 0.5 && arriving.y > 1.2,
            "the arrival should start as a thin column: {arriving:?}"
        );

        // …and once it is over the holder is back at exactly its spawn identity,
        // not merely near it.
        step(&mut app, TELEPORT_ARRIVE_SECS);
        assert_eq!(
            scale_of(&app),
            Vec3::ONE,
            "the holder must be restored on the frame the arrival ends"
        );
        // Still identity a frame later, i.e. it settled rather than passing through.
        step(&mut app, 1.0 / 60.0);
        assert_eq!(scale_of(&app), Vec3::ONE);
    }

    /// The flight lean tips the avatar *forward* — head toward its own +Z, feet
    /// back — about the chest, and is the identity when grounded. Checked here
    /// because a pose has no other way to fail loudly: a sign slip would fly the
    /// avatar on its back and only a screenshot would say so.
    #[test]
    fn flight_lean_pitches_forward_about_the_chest() {
        assert_eq!(flight_pose(0.0, 1.0), Transform::IDENTITY);

        let pose = flight_pose(FLIGHT_LEAN, 1.0);
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
