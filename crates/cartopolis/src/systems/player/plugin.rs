//! Registration for `systems::player`: the avatar, the camera, collision,
//! picking, gizmos, interaction, touch and the soundscape. The phase each system
//! runs in is `crate::phase`'s; the edges beside them are the ones `lib.rs`
//! carried before the split.
//!
//! The avatar's *existence* is not here: `sync_local_avatar` and
//! `attach_avatar_to_bigspace` live in `lib.rs` beside the session reasoning
//! that decides it, and are registered there.

use bevy::prelude::*;

use crate::phase::ClientPhase;
use crate::systems;
use crate::systems::{avatar, network, rendering};

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<systems::touch::TouchGestures>()
            // What the camera is riding along with (`systems::camera_attach`).
            .init_resource::<systems::camera_attach::CameraAttach>()
            .init_resource::<systems::touch::UiClaimRegions>()
            // What an agent driving this client asked for (`systems::mcp`); read
            // beside the on-screen controls by `avatar::handle_avatar_movement`.
            .init_resource::<avatar::AgentWalk>()
            // "Walk me there" (`systems::walk_guide`), read beside it.
            .init_resource::<avatar::RouteWalk>()
            .init_resource::<systems::gizmo::GizmoState>()
            .init_resource::<systems::picking::PrimClick>()
            .init_resource::<systems::collision::BuildingColliders>()
            .init_resource::<systems::interact::InteractState>()
            .init_resource::<crate::grid::StreamCentre>();

        app.add_systems(Startup, systems::free_camera::setup_camera);

        // Touch → gestures + the emulated mouse (`systems::touch`). MUST run after
        // `InputSystems`, which is what fills `Touches` from this frame's events;
        // unordered it would read last frame's fingers and report every delta one
        // frame late. Everything downstream is an ordinary `Update` reader, so no
        // further ordering is needed.
        app.add_systems(
            PreUpdate,
            systems::touch::recognize_touch_gestures.after(bevy::input::InputSystems),
        );
        // A preset picked in Settings swaps the render budget before anything in
        // `Update` reads it (`quality::follow_preset` says why it cannot be later).
        app.add_systems(PreUpdate, crate::quality::follow_preset);

        // The camera's switchable render state — bloom, samples, the sky and its
        // reflections — put on before the first frame is drawn, and then kept in
        // step every frame from `Update` (`MapPlugin`). `find_env_map_component`
        // goes first so it sees the camera once without reflections
        // (`render_features::EnvMapComponent`).
        app.init_resource::<systems::render_features::EnvMapComponent>();
        app.add_systems(
            Startup,
            (
                systems::render_features::find_env_map_component,
                systems::render_features::apply_render_features,
                systems::render_features::apply_sky,
            )
                .chain()
                .after(systems::free_camera::setup_camera),
        );

        // Says whether the avatar is actually on screen, on every platform.
        //
        // `Last` and not `Update`, because the thing it reads is `ViewVisibility`,
        // which Bevy writes in `PostUpdate`. From `Update` it would report the
        // *previous* frame — harmless for a settled `--shot`, which is why
        // `drive_capture` gets away with it, and misleading for a live log where
        // the interesting moment is the frame the count changes.
        app.add_systems(Last, systems::avatar::log_avatar_draw_counts);

        app.add_systems(
            Update,
            (
                systems::free_camera::camera_mode_toggle,
                // The camera mode a sign-in lands in. Before the toggle so the mode
                // it writes is the one the toggle's "Follow is not on offer" guard
                // sees this frame.
                systems::free_camera::follow_on_sign_in
                    .before(systems::free_camera::camera_mode_toggle),
                // Transform gizmos.
                systems::gizmo::handle_gizmo_mode_input,
                systems::gizmo::handle_gizmo_interaction,
                // Prim selection, raycasting, marquee, keyboard shortcuts.
                // After the gizmo so a handle-drag (sets active_axis) is seen this
                // frame and the click isn't treated as a world selection.
                systems::picking::prim_picking.after(systems::gizmo::handle_gizmo_interaction),
                systems::picking::update_marquee.after(systems::picking::prim_picking),
                systems::picking::handle_delete_key,
                systems::picking::highlight_selected_prim,
                systems::picking::unhighlight_deselected_prim,
                // Before `send_network_intent`, which is what puts a triggered
                // emote on the wire this frame rather than the next.
                systems::interact::handle_interaction_input.before(network::send_network_intent),
            )
                .in_set(ClientPhase::Input),
        )
        .add_systems(
            Update,
            // ADR-029: derive geo-anchored prims' GridCell/Transform from the
            // current OSM anchor (and re-project on re-anchor) before meshing.
            (
                systems::prim_geo::place_geo_prims,
                // Physical prims: drawn where the simulator's step is taking them.
                systems::prim_geo::smooth_physical_prims,
            )
                .chain()
                .after(network::apply_network_snapshot)
                .before(rendering::spawn_prims)
                .in_set(ClientPhase::Stream),
        )
        // Prims stand on the measured ground for drawing only: the lift goes on
        // just before propagation and comes off before anything in `Update` reads
        // a prim's `Transform` again (`components::PrimLift` says why).
        .add_systems(First, systems::prim_geo::strip_prim_lift)
        .add_systems(
            PostUpdate,
            systems::prim_geo::lift_prims_onto_terrain
                .before(big_space::prelude::BigSpaceSystems::RecenterLargeTransforms)
                .before(bevy::transform::TransformSystems::Propagate),
        )
        // A followed object's position, after it has been placed and before the
        // camera rides it (`systems::camera_attach`).
        .add_systems(
            Update,
            systems::prim_geo::report_attached_prim
                .in_set(ClientPhase::Move)
                .before(systems::free_camera::camera_controls),
        )
        .add_systems(
            Update,
            (
                avatar::handle_avatar_movement.after(network::apply_network_snapshot),
                // After `send_network_intent`, which stamps this frame's
                // `predicted_velocity` that the prediction integrates.
                avatar::smooth_online_avatar_display
                    .after(network::apply_network_snapshot)
                    .after(avatar::handle_avatar_movement)
                    .after(network::send_network_intent),
                // Keep the avatar out of buildings: block walking into them, and
                // eject one that lands inside (teleport / a building streaming over
                // it). Runs after movement and before the camera follows the
                // corrected position.
                systems::collision::eject_avatar_from_buildings
                    .after(avatar::handle_avatar_movement)
                    .before(systems::free_camera::camera_controls),
                // The props the city draws are the props the avatar cannot walk
                // through. Before movement, so a bench that streamed in this frame
                // is solid on the frame it appears.
                systems::collision::sync_prop_colliders.before(avatar::handle_avatar_movement),
                // The tunnel walls, on the same terms and for the same reason: a
                // tunnel that streamed in this frame is a floor on the frame it
                // appears, and a floor with no walls is a way to walk out into
                // solid ground.
                systems::collision::sync_bore_colliders.before(avatar::handle_avatar_movement),
                // What the avatar is standing next to, and the nearest house number.
                systems::interact::track_interaction_target.after(avatar::handle_avatar_movement),
                avatar::respawn_avatar_on_appearance_change.before(avatar::spawn_avatar),
                avatar::spawn_avatar,
            )
                .in_set(ClientPhase::Move),
        )
        .add_systems(
            Update,
            (
                systems::free_camera::camera_controls.after(avatar::smooth_online_avatar_display),
                systems::free_camera::track_stream_centre
                    .after(systems::free_camera::camera_controls),
            )
                .in_set(ClientPhase::Camera),
        )
        .add_systems(
            Update,
            (
                avatar::smooth_remote_avatars.after(avatar::smooth_online_avatar_display),
                // After `send_network_intent`: the pending emote is a one-shot that
                // this system *takes*, and the wire needs it first or a wave would
                // be seen by everyone except the people watching it happen.
                avatar::update_avatar_animation
                    .after(avatar::handle_avatar_movement)
                    .after(network::send_network_intent),
                // The airborne pose the clip set doesn't have: lean the model into
                // flight. After `spawn_avatar` so a holder that appeared this frame
                // (first spawn, appearance change) is leant on the frame it exists.
                avatar::update_avatar_model_pose
                    .after(avatar::handle_avatar_movement)
                    .after(avatar::spawn_avatar),
                avatar::update_remote_avatar_animation.after(avatar::smooth_remote_avatars),
                avatar::clear_remote_emote_edges.after(avatar::update_remote_avatar_animation),
                avatar::sync_avatar_animations.after(avatar::update_avatar_animation),
                systems::picking::render_marquee.after(systems::picking::update_marquee),
                systems::gizmo::render_gizmo_visuals,
                // The first-person twin of the driver arrow: take the avatar's
                // model out of shot while the camera is inside its head. After the
                // camera, which is what decides that, and after the driver arrow,
                // which owns the avatar's visibility whenever it is drawing over it
                // — the two never both want it, and ordering them makes that
                // visible rather than merely true.
                systems::free_camera::update_first_person_avatar
                    .after(systems::free_camera::camera_controls)
                    .after(systems::driver::update_driver_arrow),
            )
                .in_set(ClientPhase::Present),
        );

        // The soundscape. Its own registration because the whole module is behind
        // the `audio` feature (ALSA headers on Linux — see Cargo.toml), and a
        // `#[cfg]` inside a system tuple would have to be repeated per entry.
        #[cfg(feature = "audio")]
        app.add_systems(Startup, systems::soundscape::setup_soundscape)
            // The arrival whoosh. After the teleport that causes it, so a jump is
            // heard on the frame it lands rather than the next one.
            .add_systems(
                Update,
                systems::soundscape::announce_teleports
                    .after(systems::geo_nav::apply_teleport)
                    .in_set(ClientPhase::Anchor),
            )
            .add_systems(
                Update,
                (
                    // Footsteps, landings and wind: each is a function of what
                    // movement just did, so all run after it.
                    systems::soundscape::play_avatar_sounds.after(avatar::handle_avatar_movement),
                    systems::soundscape::update_wind_volume.after(avatar::handle_avatar_movement),
                )
                    .in_set(ClientPhase::Present),
            );
    }
}
