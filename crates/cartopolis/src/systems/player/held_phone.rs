//! The phone in the avatar's hand: while the high-precision mode tracks the
//! phone, the local avatar holds one, and its arm follows the real hand
//! (`docs/notes/hands-and-tones.md`).
//!
//! The clip still plays the body — the idle, the walk — and only the holding
//! arm and its fingers are posed over it, every frame after the animation and
//! before the transforms are propagated ([`pose_held_phone`]):
//! `cartopolis_human::reach` turns the upper arm and the forearm so the wrist
//! lands where the phone is, the hand is turned to the phone's own turn, and
//! the fingers close round it (`hands::phone_grip`). A small phone is drawn
//! in the hand.
//!
//! **Only this client's avatar, and only its own view of it.** Nothing here
//! goes on the wire: other people see the clip's arm.
//!
//! **Where the phone is, relative to the body** ([`track_held_phone`]). The
//! tracker says where the phone is on the map to centimetres relative to
//! where it locked, and the avatar stands on the GNSS fix, metres off; so
//! the hand is not placed on the map but moved by the tracker's motion from a
//! rest point in front of the chest, turned into the body's frame. A hand
//! carried beyond arm's reach drags that rest point along, so walking with
//! the phone keeps it in the hand. The phone's turn is the tracker's,
//! measured against the same north.
//!
//! `CARTO_HELD_PHONE=look` (or `selfie`) holds a phone in a fixed pose
//! without the mode, for a capture: `look` in front of the chest as when
//! reading the screen, `selfie` at arm's length before the face.

use bevy::prelude::*;
use cartopolis_human::hands::PhoneGrip;
use cartopolis_human::reach::{reach, Arm};

use crate::components::Avatar;
use crate::systems::avatar::AvatarHolder;
use crate::systems::prim_hand::PreciseHand;

/// Which hand holds the phone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    Left,
    #[default]
    Right,
}

impl Side {
    fn index(self) -> usize {
        match self {
            Side::Left => 0,
            Side::Right => 1,
        }
    }

    /// The body's own x toward this side: a body faces +z with its left at +x.
    fn outward(self) -> f32 {
        match self {
            Side::Left => 1.0,
            Side::Right => -1.0,
        }
    }
}

/// What the hand button remembers (`UserData::hand`).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct HandPrefs {
    /// The phone is held in the left hand.
    #[serde(default)]
    pub left: bool,
    /// The first-use card was answered with "Use my hand".
    #[serde(default)]
    pub agreed: bool,
}

impl HandPrefs {
    #[must_use]
    pub fn side(&self) -> Side {
        if self.left {
            Side::Left
        } else {
            Side::Right
        }
    }
}

/// The hand button's first-use card is up (`map_osd::hand_card`).
///
/// **No prompt offers the hand by a prim that senses hands** (dropped
/// 2026-10-28, at the user's word): a theremin is a script, and what it does
/// with a hand is its own business, not the interface's.
#[derive(Resource, Default)]
pub struct HandCard(pub bool);

/// A fixed pose to hold a phone in, for a capture (`CARTO_HELD_PHONE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandIn {
    /// In front of the chest, the screen turned up to the eyes.
    Look,
    /// At arm's length before the face, the screen (and the front camera)
    /// toward it.
    Selfie,
}

impl StandIn {
    fn from_env() -> Option<Self> {
        match std::env::var("CARTO_HELD_PHONE").ok()?.as_str() {
            "1" | "look" => Some(Self::Look),
            "selfie" => Some(Self::Selfie),
            _ => None,
        }
    }

    /// Where the phone is from the holding shoulder, in the body's frame,
    /// with `outward` the side's x.
    fn from_shoulder(self, outward: f32) -> Vec3 {
        match self {
            Self::Look => Vec3::new(-outward * 0.16, -0.12, 0.32),
            Self::Selfie => Vec3::new(-outward * 0.12, 0.2, 0.46),
        }
    }
}

/// The phone, in the holder's frame: metres from the soles, +z ahead, +x to
/// the body's left; and its turn from its own axes (x right, y up the screen,
/// z out of the screen) to the holder's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhoneInHand {
    pub at: Vec3,
    pub rotation: Quat,
}

/// The phone the local avatar holds, if it holds one.
#[derive(Resource)]
pub struct HeldPhone {
    /// The holding hand, copied each frame from the user's choice
    /// (`UserData::hand`, the hand button's menu).
    pub side: Side,
    /// This frame's phone; `None` leaves the clip's arm.
    pub phone: Option<PhoneInHand>,
    stand_in: Option<StandIn>,
    /// The tracked hand on the map, and where in the body's frame it was put:
    /// the hand is moved from there by the tracker's motion.
    reference: Option<((f64, f64, f32), Vec3)>,
    /// The phone is held up for a selfie: the avatar turns to face the camera
    /// (`avatar::update_avatar_model_pose`).
    pub selfie: bool,
    /// The eyes above the soles, metres: where a selfie's phone looks.
    pub eyes_height: f32,
    /// The phone's attitude while the camera screen has a camera, which takes
    /// it from the tracker (`CameraScreen::phone_hold`).
    compass: Option<crate::platform::orientation::OrientationWatch>,
}

impl HeldPhone {
    /// The fixed pose a capture holds a phone in, if one does.
    #[must_use]
    pub fn stand_in(&self) -> Option<StandIn> {
        self.stand_in
    }
}

impl Default for HeldPhone {
    fn default() -> Self {
        Self {
            side: Side::default(),
            phone: None,
            stand_in: StandIn::from_env(),
            reference: None,
            selfie: false,
            eyes_height: 1.6,
            compass: None,
        }
    }
}

/// How far a selfie holds the phone from the eyes, metres: an arm held out.
/// The distance the front camera's view of the face will measure, until then
/// a typical one.
pub const SELFIE_REACH_M: f32 = 0.5;

/// How far a phone is held from the eyes to take a picture facing out.
const PICTURE_REACH_M: f32 = 0.35;

/// The way the phone's screen faces, in the world's axes, from an attitude:
/// toward the eyes of whoever holds it. A selfie's camera is the eyes less
/// this times [`SELFIE_REACH_M`].
#[must_use]
pub fn screen_out(attitude: &crate::platform::orientation::DeviceAttitude) -> Vec3 {
    enu_to_world() * attitude.screen_to_world.as_quat() * Vec3::Z
}

/// The holding arm's joints and the hand's grip, kept on a body's holder.
/// Made with the body (`human_avatar::spawn_human`).
#[derive(Component, Clone)]
pub struct HumanArms {
    /// Per side, left first.
    pub arms: [Option<ArmJoints>; 2],
    /// The head at rest, in the holder's frame: where the eyes are looked
    /// for.
    pub head: Vec3,
    /// The phone drawn in the hand, hidden while nothing is held.
    pub phone: Entity,
}

/// One arm of a body: its joints and how its hand holds a phone.
#[derive(Clone)]
pub struct ArmJoints {
    pub clavicle: Entity,
    pub upper: Entity,
    pub lower: Entity,
    pub hand: Entity,
    pub fingers: Vec<(Entity, Quat)>,
    /// The hand bone's axes toward the fingers and out of the palm.
    pub towards_fingers: Vec3,
    pub palm: Vec3,
    /// The middle knuckle from the wrist, in the hand bone's frame.
    pub to_knuckle: Vec3,
    /// The shoulder at rest, in the holder's frame, and the arm's length to
    /// the wrist.
    pub shoulder: Vec3,
    pub length: f32,
}

impl ArmJoints {
    /// Bind a grip to a body's joint entities, by the rig's names.
    #[must_use]
    pub fn new(
        side: Side,
        grip: &PhoneGrip,
        joint: impl Fn(&str) -> Option<(Entity, Vec3)>,
        entity: impl Fn(usize) -> Option<Entity>,
    ) -> Option<Self> {
        let s = match side {
            Side::Left => "l",
            Side::Right => "r",
        };
        let (clavicle, _) = joint(&format!("clavicle_{s}"))?;
        let (upper, shoulder) = joint(&format!("upperarm_{s}"))?;
        let (lower, elbow) = joint(&format!("lowerarm_{s}"))?;
        let (hand, wrist) = joint(&format!("hand_{s}"))?;
        Some(Self {
            clavicle,
            upper,
            lower,
            hand,
            fingers: grip
                .fingers
                .iter()
                .filter_map(|&(i, q)| Some((entity(i)?, q)))
                .collect(),
            towards_fingers: grip.towards_fingers,
            to_knuckle: grip.to_knuckle,
            palm: grip.palm,
            shoulder,
            length: shoulder.distance(elbow) + elbow.distance(wrist),
        })
    }
}

/// The phone's own axes in the hand, per side: which way, from the palm, the
/// fingers point. A phone held to be read lies across the palm, the fingers
/// round its far edge and a little up it.
fn fingers_along(side: Side) -> Vec3 {
    Vec3::new(side.outward(), 0.35, 0.0).normalize()
}

/// Where the middle knuckle sits on the phone, from its centre in its own
/// axes: behind the back, a couple of centimetres in from the edge the
/// fingers curl round, below the middle. The palm then lies across the back
/// toward the holding side, and the wrist past its edge. Placing the wrist
/// at a guessed offset instead left the phone beside the hand.
fn knuckle_on_phone(side: Side) -> Vec3 {
    Vec3::new(side.outward() * 0.018, -0.03, -0.02)
}

/// How far a hand carried away drags its rest point: past this share of the
/// arm's length the point follows it.
const LEASH: f32 = 0.95;

/// The phone drawn in the hand, metres: a phone's width, height and depth.
pub const PHONE_SIZE: Vec3 = Vec3::new(0.075, 0.16, 0.009);

/// Seconds for the arm to come up to the phone or go back to the clip.
const BLEND_S: f32 = 0.25;

/// East-north-up to the world's axes (+x east, +y up, +z south).
fn enu_to_world() -> Quat {
    Quat::from_mat3(&Mat3::from_cols(Vec3::X, Vec3::NEG_Z, Vec3::Y))
}

/// A phone at `at` whose screen faces `eyes`, upright.
fn facing(at: Vec3, eyes: Vec3) -> Quat {
    let z = (eyes - at).normalize_or(Vec3::NEG_Z);
    let x = Vec3::Y.cross(z).normalize_or(Vec3::X);
    Quat::from_mat3(&Mat3::from_cols(x, z.cross(x), z))
}

/// Decide where the phone is this frame.
pub fn track_held_phone(
    precise: Res<PreciseHand>,
    store: Res<crate::systems::user_store::UserStore>,
    screen: Res<crate::systems::camera_screen::CameraScreen>,
    mut held: ResMut<HeldPhone>,
    avatars: Query<(), With<Avatar>>,
    holders: Query<(&ChildOf, &GlobalTransform, &HumanArms), With<AvatarHolder>>,
) {
    let side = store.data.hand.side();
    held.side = side;
    let hold = screen.phone_hold();
    held.selfie = hold == Some(true);
    let Some((_, body, arms)) = holders
        .iter()
        .find(|(c, _, _)| avatars.contains(c.parent()))
    else {
        held.phone = None;
        held.reference = None;
        return;
    };
    let Some(arm) = &arms.arms[side.index()] else {
        held.phone = None;
        return;
    };
    let eyes = arms.head + Vec3::new(0.0, 0.08, 0.1);
    held.eyes_height = eyes.y;
    let to_body = body.rotation().inverse();
    // The camera screen has a camera (the photograph's, or the front one
    // turned to the avatar): the tracker cannot have it, and the phone is
    // placed by how it is turned instead — its screen toward the eyes, at
    // arm's length for a selfie (`docs/mockups/hand-tracking/` v2).
    if let Some(front) = hold {
        held.reference = None;
        let attitude = held
            .compass
            .get_or_insert_with(crate::platform::orientation::OrientationWatch::start)
            .latest();
        held.phone = Some(match attitude {
            Some(attitude) => {
                let rotation =
                    (to_body * enu_to_world() * attitude.screen_to_world.as_quat()).normalize();
                let reach = if front {
                    SELFIE_REACH_M
                } else {
                    PICTURE_REACH_M
                };
                PhoneInHand {
                    at: eyes - rotation * Vec3::Z * reach,
                    rotation,
                }
            }
            // No motion sensors (a desktop): the pose a capture would hold.
            None => {
                let pose = if front {
                    StandIn::Selfie
                } else {
                    StandIn::Look
                };
                let at = arm.shoulder + pose.from_shoulder(side.outward());
                PhoneInHand {
                    at,
                    rotation: facing(at, eyes),
                }
            }
        });
        return;
    }
    held.compass = None;
    let tracked = precise
        .active
        .then_some(precise.hand_geo.zip(precise.hand_rotation))
        .flatten();
    let Some((geo, turn)) = tracked else {
        held.reference = None;
        held.phone = held.stand_in.map(|pose| {
            let at = arm.shoulder + pose.from_shoulder(side.outward());
            PhoneInHand {
                at,
                rotation: facing(at, eyes),
            }
        });
        return;
    };
    let rest = arm.shoulder + StandIn::Look.from_shoulder(side.outward());
    let (origin, from) = *held.reference.get_or_insert((geo, rest));
    let (per_lat, per_lng) = cartopolis_core::degree_scale_m(origin.0);
    let moved = Vec3::new(
        ((geo.1 - origin.1) * per_lng) as f32,
        geo.2 - origin.2,
        -((geo.0 - origin.0) * per_lat) as f32,
    );
    let mut at = from + to_body * moved;
    let reach_m = arm.length * LEASH;
    let out = at - arm.shoulder;
    if out.length() > reach_m {
        let kept = arm.shoulder + out.normalize() * reach_m;
        if let Some((_, from)) = held.reference.as_mut() {
            *from += kept - at;
        }
        at = kept;
    }
    held.phone = Some(PhoneInHand {
        at,
        rotation: (to_body * enu_to_world() * turn).normalize(),
    });
}

/// A joint's pose in its holder's frame, from the transforms as the animation
/// left them this frame.
fn in_holder(
    entity: Entity,
    holder: Entity,
    joints: &Query<(&mut Transform, &ChildOf), Without<HeldPhoneMesh>>,
) -> Option<(Vec3, Quat)> {
    let (mut at, mut rotation) = (Vec3::ZERO, Quat::IDENTITY);
    let mut e = entity;
    while e != holder {
        let (t, parent) = joints.get(e).ok()?;
        at = t.translation + t.rotation * at;
        rotation = t.rotation * rotation;
        e = parent.parent();
    }
    Some((at, rotation))
}

/// Marks the phone drawn in a hand.
#[derive(Component)]
pub struct HeldPhoneMesh;

/// How far the arm is on its way to the phone (1) or back to the clip (0).
#[derive(Resource, Default)]
pub struct HeldPhoneBlend(f32);

/// Pose the holding arm and fingers over the clip, and draw the phone.
pub fn pose_held_phone(
    time: Res<Time>,
    held: Res<HeldPhone>,
    mut blend: ResMut<HeldPhoneBlend>,
    avatars: Query<(), With<Avatar>>,
    holders: Query<(Entity, &ChildOf, &HumanArms), With<AvatarHolder>>,
    mut joints: Query<(&mut Transform, &ChildOf), Without<HeldPhoneMesh>>,
    mut phones: Query<(&mut Transform, &mut Visibility), With<HeldPhoneMesh>>,
) {
    let step = time.delta_secs() / BLEND_S;
    blend.0 = if held.phone.is_some() {
        (blend.0 + step).min(1.0)
    } else {
        (blend.0 - step).max(0.0)
    };
    let w = blend.0;
    for (holder, parent, arms) in &holders {
        if !avatars.contains(parent.parent()) {
            continue;
        }
        if let Ok((_, mut shown)) = phones.get_mut(arms.phone) {
            let want = if held.phone.is_some() && w > 0.5 {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
            if *shown != want {
                *shown = want;
            }
        }
        let (Some(phone), Some(arm)) = (held.phone, &arms.arms[held.side.index()]) else {
            continue;
        };
        if w <= 0.0 {
            continue;
        }
        let pose = |e| in_holder(e, holder, &joints);
        let (Some(clavicle), Some(upper), Some(lower), Some(hand)) = (
            pose(arm.clavicle),
            pose(arm.upper),
            pose(arm.lower),
            pose(arm.hand),
        ) else {
            continue;
        };
        let side = held.side;
        // The hand turned so its fingers and palm lie as the phone says.
        let fingers = phone.rotation * fingers_along(side);
        let palm = phone.rotation * Vec3::Z;
        let palm = (palm - fingers * palm.dot(fingers)).normalize_or(Vec3::Z);
        let local_palm =
            (arm.palm - arm.towards_fingers * arm.palm.dot(arm.towards_fingers)).normalize();
        let basis = |f: Vec3, p: Vec3| Quat::from_mat3(&Mat3::from_cols(f, p, f.cross(p)));
        let hand_rot = basis(fingers, palm) * basis(arm.towards_fingers, local_palm).inverse();
        // And placed so its knuckles are on the phone's back.
        let knuckle = phone.at + phone.rotation * knuckle_on_phone(side);
        let wrist = knuckle - hand_rot * arm.to_knuckle;
        let r = reach(
            &Arm {
                shoulder: upper.0,
                elbow: lower.0,
                wrist: hand.0,
            },
            wrist,
            Vec3::new(side.outward(), -1.0, -0.3),
        );
        let upper_rot = r.upper * upper.1;
        let lower_rot = r.lower * r.upper * lower.1;

        let locals = [
            (arm.upper, clavicle.1.inverse() * upper_rot),
            (arm.lower, upper_rot.inverse() * lower_rot),
            (arm.hand, lower_rot.inverse() * hand_rot),
        ];
        for (e, solved) in locals.into_iter().chain(arm.fingers.iter().copied()) {
            if let Ok((mut t, _)) = joints.get_mut(e) {
                t.rotation = t.rotation.slerp(solved.normalize(), w);
            }
        }
        if let Ok((mut t, _)) = phones.get_mut(arms.phone) {
            *t = Transform::from_translation(phone.at).with_rotation(phone.rotation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_facing_the_eyes_turns_its_screen_to_them_and_stands_upright() {
        let at = Vec3::new(-0.1, 1.2, 0.35);
        let eyes = Vec3::new(0.0, 1.65, 0.1);
        let q = facing(at, eyes);
        assert!((q * Vec3::Z).dot((eyes - at).normalize()) > 0.9999);
        assert!(
            (q * Vec3::X).y.abs() < 1e-5,
            "the screen is not tipped sideways"
        );
        assert!((q * Vec3::Y).y > 0.0, "its top is up");
    }

    #[test]
    fn a_screen_facing_north_faces_minus_z() {
        // The screen's own +z turned onto north: a quarter turn about east.
        let attitude = crate::platform::orientation::DeviceAttitude {
            screen_to_world: bevy::math::DQuat::from_rotation_x(-std::f64::consts::FRAC_PI_2),
            accuracy_deg: None,
        };
        let out = screen_out(&attitude);
        assert!((out - Vec3::NEG_Z).length() < 1e-5, "{out}");
        // A selfie's camera is then half a metre south of the eyes, looking
        // north at them.
        let eyes = Vec3::new(0.0, 1.6, 0.0);
        let camera = eyes - out * SELFIE_REACH_M;
        assert!((camera - Vec3::new(0.0, 1.6, 0.5)).length() < 1e-5);
    }

    #[test]
    fn east_north_up_becomes_the_worlds_axes() {
        let q = enu_to_world();
        assert!((q * Vec3::X - Vec3::X).length() < 1e-6, "east");
        assert!((q * Vec3::Y - Vec3::NEG_Z).length() < 1e-6, "north is −z");
        assert!((q * Vec3::Z - Vec3::Y).length() < 1e-6, "up");
    }

    #[test]
    fn the_stand_in_poses_are_in_reach_of_a_mans_arm() {
        // A 1.75 m man's arm is about 56 cm to the wrist, and the wrist is
        // nearer the shoulder than the phone it holds.
        for pose in [StandIn::Look, StandIn::Selfie] {
            for side in [Side::Left, Side::Right] {
                let phone = pose.from_shoulder(side.outward());
                assert!(
                    phone.length() < 0.56,
                    "{pose:?} {side:?}: {}",
                    phone.length()
                );
                // In front of the body and toward its middle.
                assert!(phone.z > 0.3 && phone.x * side.outward() < 0.0);
            }
        }
    }
}
