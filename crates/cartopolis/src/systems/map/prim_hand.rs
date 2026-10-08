//! Reporting this user's hand to the prims that sense hands (protocol 36,
//! `sense_hands` / `on_hand`). The client's half of `cartopolis_core::tone`.
//!
//! A prim whose script asked for hands carries a range
//! (`components::SensesHands`); while the hand is within it, this reports where
//! the hand is relative to the prim ([`HandPose`]), at most [`HAND_HZ`] per prim.
//! What a hand means — a note, a door, a game — and whose counts is the
//! script's business; this module does not know which.
//!
//! **The hand is the camera, and that is only true with high precision on.**
//! The phone's tracked pose *is* the view, so where the camera is is where the
//! hand holding the phone is, to centimetres. Switching that mode on is also
//! the consent: nothing is reported otherwise, because a hand report is a
//! precise position report and nobody agreed to send one by walking past a
//! prim. The developer switch — Developer tools ▸ Precision ▸ *Stand-in hand*,
//! or `CARTO_HAND_FROM_CAMERA=1` at launch — lets something else stand in,
//! reported as `precise = false` so a script can tell: the **avatar's hand**
//! (chest height above it) when there is an avatar, so walking up to a
//! theremin's rod plays it on a phone; the view otherwise, as in fly mode.

use std::collections::HashMap;

use bevy::prelude::*;
use cartopolis_core::tone::HAND_HZ;
use cartopolis_core::{HandPose, NetMessage};

use crate::components::{FreeCamera, Prim, SensesHands};
use crate::resources::OnlineSession;

/// Whether the view is the hand: set by the high-precision tracking mode while
/// it holds a camera-tracked pose. Until that mode exists this stays `false`.
#[derive(Resource)]
pub struct PreciseHand {
    pub active: bool,
    /// Where the hand is while `active`: latitude, longitude and metres above
    /// the ground, from the high-precision mode's tracker anchored on its first
    /// GNSS fix and compass heading (`systems::high_precision`).
    pub hand_geo: Option<(f64, f64, f32)>,
    /// Report a stand-in as an imprecise hand — the avatar's, or the view — so
    /// a hand-sensing script can be tried before high precision positions
    /// anything. `CARTO_HAND_FROM_CAMERA=1` sets it at launch; Developer tools
    /// ▸ Precision switches it while the app runs.
    pub camera_stands_in: bool,
}

impl Default for PreciseHand {
    fn default() -> Self {
        Self {
            active: false,
            hand_geo: None,
            camera_stands_in: std::env::var("CARTO_HAND_FROM_CAMERA").is_ok_and(|v| v == "1"),
        }
    }
}

impl PreciseHand {
    /// Whether a hand may be reported at all, and if so whether it is precise.
    #[must_use]
    pub fn reporting(&self) -> Option<bool> {
        if self.active {
            Some(true)
        } else if self.camera_stands_in {
            Some(false)
        } else {
            None
        }
    }
}

/// What was last reported to each prim, and how long ago.
#[derive(Resource, Default)]
pub struct HandReports {
    last: HashMap<i64, ([f32; 3], f32)>,
    /// The tracked hand this frame, copied from [`PreciseHand::hand_geo`].
    hand_geo: Option<(f64, f64, f32)>,
}

/// Re-send an unchanged hand this often, so a script timing out a hand that
/// stopped arriving does not drop one that is merely still.
const KEEPALIVE_S: f32 = 0.5;
/// A hand that moved less than this since the last report is not reported
/// again (until the keepalive): a millimetre is under anything a hand does on
/// purpose.
const MIN_MOVE_M: f32 = 0.001;

/// How far above the avatar's feet its stand-in hand is: chest height, where a
/// hand held out in front of you would be.
const STAND_IN_HAND_HEIGHT_M: f32 = 1.2;

/// Where `hand` is in `prim`'s own frame: turned with it, not scaled with it.
#[must_use]
pub fn hand_in_prim_frame(prim: &GlobalTransform, hand: Vec3) -> Vec3 {
    let (_, rotation, translation) = prim.to_scale_rotation_translation();
    rotation.inverse() * (hand - translation)
}

/// Report the hand to every prim that senses hands within reach of it.
pub fn report_hands(
    time: Res<Time>,
    precise: Res<PreciseHand>,
    online: Option<Res<OnlineSession>>,
    camera: Query<&GlobalTransform, With<FreeCamera>>,
    avatar: Query<&GlobalTransform, (With<crate::components::Avatar>, Without<FreeCamera>)>,
    prims: Query<(
        &Prim,
        &SensesHands,
        &GlobalTransform,
        &crate::components::PrimGeo,
        &Transform,
    )>,
    anchor: Res<crate::resources::MapAnchor>,
    mut reports: ResMut<HandReports>,
) {
    let dt = time.delta_secs();
    reports.hand_geo = precise.hand_geo.filter(|_| precise.active);
    let (Some(precise), Some(session), Ok(view)) = (precise.reporting(), online, camera.single())
    else {
        reports.last.clear();
        return;
    };
    // With high precision the view is the phone in the hand. A stand-in is the
    // avatar's hand where there is an avatar, the view where there is not.
    let eye = match (precise, avatar.single()) {
        (false, Ok(body)) => body.translation() + Vec3::Y * STAND_IN_HAND_HEIGHT_M,
        _ => view.translation(),
    };
    // With a tracked hand, the hand is measured against each prim through their
    // geo anchors rather than the view: the tracker knows where the phone is on
    // the map, and the camera is wherever the user left it.
    let tracked = if precise {
        anchor.geo.zip(reports.hand_geo)
    } else {
        None
    };
    let mut near: Vec<i64> = Vec::new();
    for (prim, senses, at, geo, local) in &prims {
        if senses.0 <= 0.0 {
            continue;
        }
        let hand = match tracked {
            Some((map, (lat, lng, alt))) => {
                let (hx, hz) = crate::systems::geo_nav::lat_lng_to_world(map, lat, lng);
                let (px, pz) =
                    crate::systems::geo_nav::lat_lng_to_world(map, geo.latitude, geo.longitude);
                let offset = Vec3::new(
                    (hx - px) as f32,
                    alt - local.translation.y,
                    (hz - pz) as f32,
                );
                let (_, rotation, _) = at.to_scale_rotation_translation();
                rotation.inverse() * offset
            }
            None => hand_in_prim_frame(at, eye),
        };
        if hand.length() > senses.0 {
            continue;
        }
        near.push(prim.id);
        let (last, since) = reports
            .last
            .entry(prim.id)
            .or_insert(([f32::INFINITY; 3], f32::INFINITY));
        *since += dt;
        if *since < 1.0 / HAND_HZ {
            continue;
        }
        let moved = Vec3::from_array(*last).distance(hand) > MIN_MOVE_M;
        if moved || *since >= KEEPALIVE_S {
            // Per-frame traffic, so straight onto the channel like a movement
            // intent: a dropped hand is corrected by the next one.
            let _ = session.intent_tx.unbounded_send(NetMessage::HandPose {
                pose: HandPose {
                    prim_id: prim.id,
                    offset: hand.to_array(),
                    precise,
                },
            });
            *last = hand.to_array();
            *since = 0.0;
        }
    }
    // A hand that left a prim's range starts afresh if it comes back.
    reports.last.retain(|id, _| near.contains(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hand_is_measured_in_the_prims_own_frame() {
        let prim = GlobalTransform::from(
            Transform::from_translation(Vec3::new(10.0, 1.0, 0.0))
                .with_rotation(Quat::from_rotation_y(std::f32::consts::PI))
                .with_scale(Vec3::splat(3.0)),
        );
        // Half a metre west of a prim turned half round is half a metre along
        // its own +x — and not a sixth of that for the prim being scaled.
        let hand = hand_in_prim_frame(&prim, Vec3::new(9.5, 1.2, 0.0));
        assert!((hand - Vec3::new(0.5, 0.2, 0.0)).length() < 1e-5, "{hand}");
    }

    #[test]
    fn nothing_is_reported_without_consent() {
        let off = PreciseHand {
            active: false,
            hand_geo: None,
            camera_stands_in: false,
        };
        assert_eq!(off.reporting(), None);
        let dev = PreciseHand {
            active: false,
            hand_geo: None,
            camera_stands_in: true,
        };
        assert_eq!(dev.reporting(), Some(false));
        let tracked = PreciseHand {
            active: true,
            hand_geo: None,
            camera_stands_in: true,
        };
        assert_eq!(tracked.reporting(), Some(true));
    }
}
