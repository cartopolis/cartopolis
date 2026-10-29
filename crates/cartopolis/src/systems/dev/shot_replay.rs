//! A recorded scene played back in a scripted capture: the prims a simulator
//! sent and when, and a hand near one of them, on the movie's clock
//! (`systems::anim_clock`). `CARTO_SHOT_REPLAY=<file>` names the recording.
//!
//! It exists because a scripted run is always offline: it has no simulator, so
//! no prims and no script, and the local avatar's arm follows only the
//! high-precision tracker. A recording carries both in from a run that had
//! them — `prim_tone`'s `virtual_player` test writes one beside the WAV of the
//! theremin it plays — so a capture can show the avatar playing what was
//! heard, the hand drawn where the script sensed it.
//!
//! ```json
//! { "prims": [{ "t": 0.49, "prim": { …PrimDto… } }],
//!   "hand":  [{ "t": 0.45, "prim_id": 3, "offset": [0.6, 0.45, 0.0] }] }
//! ```
//!
//! Seconds from the recording's start, which is the movie's first frame. A
//! prim is spawned, or updated as a `PrimUpsert` updates it, once the clock
//! passes its `t`. A hand is where `HandPose::offset` says, in that prim's
//! frame, between two samples less than [`HAND_GAP_S`] apart; outside them the
//! arm goes back to the clip. While a hand is in the recording the avatar
//! faces the prim it plays.

use bevy::prelude::*;
use cartopolis_core::PrimDto;
use serde::Deserialize;

use crate::components::{Avatar, Prim};
use crate::systems::anim_clock::AnimClock;
use crate::systems::avatar::AvatarHolder;

/// Two hand samples further apart than this are a hand that left between them,
/// not one to draw moving across the gap: a client reports at 20 Hz.
pub const HAND_GAP_S: f32 = 0.15;

#[derive(Deserialize, Clone)]
struct PrimAt {
    t: f32,
    prim: PrimDto,
}

#[derive(Deserialize, Clone, Copy)]
struct HandAt {
    t: f32,
    prim_id: i64,
    offset: [f32; 3],
}

#[derive(Deserialize)]
struct Recording {
    #[serde(default)]
    prims: Vec<PrimAt>,
    #[serde(default)]
    hand: Vec<HandAt>,
}

/// The recording being played.
#[derive(Resource)]
pub struct ShotReplay {
    prims: Vec<PrimAt>,
    next: usize,
    hand: Vec<HandAt>,
}

impl ShotReplay {
    /// The recording `CARTO_SHOT_REPLAY` names, if it does. A file that does
    /// not read is a mistake in the command, so it says so and plays nothing.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("CARTO_SHOT_REPLAY")?;
        let rec: Recording = match std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        {
            Ok(rec) => rec,
            Err(e) => {
                tracing::error!(path = %path.to_string_lossy(), "shot replay unreadable: {e}");
                return None;
            }
        };
        let mut prims = rec.prims;
        prims.sort_by(|a, b| a.t.total_cmp(&b.t));
        let mut hand = rec.hand;
        hand.sort_by(|a, b| a.t.total_cmp(&b.t));
        tracing::info!(prims = prims.len(), hand = hand.len(), "shot replay loaded");
        Some(Self {
            prims,
            next: 0,
            hand,
        })
    }
}

/// The hand at `t`: the prim it is near and its offset there, between the two
/// samples around `t`.
fn hand_at(hand: &[HandAt], t: f32) -> Option<(i64, [f32; 3])> {
    let i = hand.partition_point(|h| h.t <= t);
    let (a, b) = (hand.get(i.checked_sub(1)?)?, hand.get(i)?);
    if b.t - a.t > HAND_GAP_S || a.prim_id != b.prim_id {
        return None;
    }
    let k = ((t - a.t) / (b.t - a.t).max(1e-6)).clamp(0.0, 1.0);
    let lerp = |j: usize| a.offset[j] + (b.offset[j] - a.offset[j]) * k;
    Some((a.prim_id, [lerp(0), lerp(1), lerp(2)]))
}

/// Spawn and update the recording's prims as the clock passes them, and put
/// its hand in the world.
pub fn play_shot_replay(
    mut commands: Commands,
    clock: Res<AnimClock>,
    mut replay: ResMut<ShotReplay>,
    mut held: ResMut<crate::systems::held_phone::HeldPhone>,
    mut prims: Query<(Entity, &mut Prim, &mut Transform, &GlobalTransform)>,
    mut avatar_state: ResMut<crate::resources::AvatarState>,
    avatars: Query<(), With<Avatar>>,
    holders: Query<(&ChildOf, &GlobalTransform), (With<AvatarHolder>, Without<Prim>)>,
) {
    let t = clock.elapsed_secs();
    while let Some(due) = replay.prims.get(replay.next).filter(|p| p.t <= t).cloned() {
        replay.next += 1;
        match prims.iter_mut().find(|(_, p, _, _)| p.id == due.prim.id) {
            Some((entity, mut prim, mut transform, _)) => {
                crate::systems::network::update_prim_from_dto(
                    &mut commands,
                    entity,
                    &mut prim,
                    &mut transform,
                    &due.prim,
                );
            }
            None => crate::systems::network::spawn_prim_from_dto(&mut commands, due.prim),
        }
    }

    held.placed = None;
    let Some((id, offset)) = hand_at(&replay.hand, t) else {
        return;
    };
    let Some((_, _, _, at)) = prims.iter().find(|(_, p, _, _)| p.id == id) else {
        return;
    };
    // The prim's own frame, turned with it and not scaled, as `HandPose` is.
    let (_, rotation, centre) = at.to_scale_rotation_translation();
    held.placed = Some(centre + rotation * Vec3::from(offset));
    if let Some((_, body)) = holders.iter().find(|(c, _)| avatars.contains(c.parent())) {
        let to = centre - body.translation();
        // A body's forward is (sin yaw, cos yaw) in x and z.
        let facing = to.x.atan2(to.z);
        if (avatar_state.facing_yaw - facing).abs() > 1e-3 {
            avatar_state.facing_yaw = facing;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t: f32, x: f32) -> HandAt {
        HandAt {
            t,
            prim_id: 1,
            offset: [x, 0.0, 0.0],
        }
    }

    #[test]
    fn a_hand_moves_between_samples_and_is_gone_across_a_gap() {
        let hand = [at(0.0, 0.0), at(0.1, 1.0), at(1.0, 5.0)];
        assert_eq!(hand_at(&hand, 0.05), Some((1, [0.5, 0.0, 0.0])));
        // Before the first and after the last there is no hand, and none
        // between two samples a second apart.
        assert_eq!(hand_at(&hand, -0.1), None);
        assert_eq!(hand_at(&hand, 0.5), None);
        assert_eq!(hand_at(&hand, 1.1), None);
    }
}
