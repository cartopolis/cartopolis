//! The local avatar's existence: spawned with a session (or for a capture that
//! photographs one), despawned with it, and put into the `BigSpace` grid.
//! Registered by `lib.rs`'s `ShellPlugin`.

use bevy::prelude::*;
use big_space::prelude::{BigSpace, GridCell};

use crate::components::Avatar;
use crate::systems;

/// **The avatar belongs to a session, and offline there is not one.**
///
/// It used to be spawned once at startup and never despawned, so `--offline` —
/// which is the default, and every scripted run — put a body on the map with no
/// sim behind it: nothing owned its position, nobody else could see it, and the
/// Follow camera that is its whole point was refused
/// ([`free_camera::follow_available`]). Offline is the streamed map and a camera.
///
/// The one exception is the shot harness, and it is not a loophole: a scripted
/// run is always offline, so `--shot-mode avatar`, `--ui sit` and `--ui driver`
/// would otherwise be photographing an empty street. The harness already states
/// what each shot is of, so [`ShotHarness::wants_avatar`] reads that rather than
/// taking a flag of its own — a capture cannot ask for an avatar without saying
/// what it wants one for.
///
/// Written as a sync rather than a pair of one-shot systems because the session
/// is switched at runtime in both directions (`systems::session`), and `connect`
/// and `teardown` are already an exact pair without a third thing to keep in
/// step.
pub fn sync_local_avatar(
    mut commands: Commands,
    session: Res<systems::session::SessionState>,
    harness: Option<Res<systems::shot_harness::ShotHarness>>,
    avatars: Query<Entity, With<Avatar>>,
) {
    let wanted = session.is_online() || harness.is_some_and(|h| h.wants_avatar());
    let present = !avatars.is_empty();
    if wanted == present {
        return;
    }
    if wanted {
        // Scale is applied by `avatar::spawn_avatar` from the resolved model,
        // before the mesh instantiates — so spawn at the identity scale here.
        // `attach_avatar_to_bigspace` gives it a `GridCell` on the same frame.
        commands.spawn((Avatar, Transform::from_translation(Vec3::ZERO)));
        return;
    }
    // `try_despawn`, and recursive: the model holders are children, and
    // `session::teardown` retires this same entity from the other side with no
    // ordering between the two.
    for e in avatars.iter() {
        commands.entity(e).try_despawn();
    }
}

/// Attach the avatar to the world `BigSpace` once it exists, so it rebases with
/// the camera instead of glitching past the first cell (ADR-019).
pub fn attach_avatar_to_bigspace(
    mut commands: Commands,
    avatars: Query<Entity, (With<Avatar>, Without<GridCell>)>,
    bigspace: Query<Entity, With<BigSpace>>,
) {
    let Ok(root) = bigspace.single() else {
        return;
    };
    for entity in avatars.iter() {
        commands
            .entity(entity)
            .insert((GridCell::default(), ChildOf(root)));
    }
}
