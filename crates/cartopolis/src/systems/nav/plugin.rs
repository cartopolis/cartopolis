//! Registration for `systems::nav`: the teleport and geocoding, route planning
//! and driver mode, device tracking, ambient traffic and live transit. The phase
//! each system runs in is `crate::phase`'s; the edges beside them are the ones
//! `lib.rs` carried before the split.
//!
//! Three of this group's systems are registered elsewhere, in the panel chain
//! (`gui::plugin`): `geo_nav::render_search_panel`, `navigation::render_route_sheet`
//! and `locate::render_locate_arrow` draw into the shared root `Ui`, and a panel
//! outside that one tuple is not in the chain.

use bevy::prelude::*;

use crate::phase::ClientPhase;
use crate::systems;
use crate::systems::avatar;
use crate::systems::stream_watch::WatchStreamExt;

pub struct NavPlugin;

impl Plugin for NavPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<systems::transit_live::TransitLive>()
            .init_resource::<systems::transit_live::WorldClock>()
            .init_resource::<systems::transit_vehicles::TransitVehicles>()
            .init_resource::<systems::geo_nav::GeoNavState>()
            .init_resource::<systems::navigation::RouteState>()
            .watch_stream::<systems::navigation::RouteState>()
            .init_resource::<systems::driver::DriverState>()
            .init_resource::<systems::walk_guide::WalkGuide>()
            .init_resource::<systems::locate::LocateTracking>()
            .init_resource::<systems::high_precision::HighPrecision>()
            .init_resource::<systems::high_precision::HighPrecisionLog>()
            .init_resource::<systems::route_ahead::RouteAhead>()
            .init_resource::<systems::traffic::DriveNetwork>();

        // The car pool. One mesh, a handful of materials and a fixed number of
        // entities, all built once — after which this layer uploads nothing
        // (`systems::traffic`).
        //
        // **After the camera**, which is what spawns the `BigSpace` root the cars
        // are parented to (`free_camera::setup_camera` calls `spawn_big_space`).
        // Registered with no ordering it ran first, found no root, returned
        // silently, and the pool was never created — no cars and nothing in the
        // log. `setup_sky_dome` carries the same `.after` for its own reason.
        app.add_systems(
            Startup,
            systems::traffic::setup_traffic.after(systems::free_camera::setup_camera),
        );

        app.add_systems(
            Update,
            (
                // A chat's "+ → Send this view": the camera's view, as a point,
                // queued for the sealing in `private_chat` to pick up on its next
                // pass.
                systems::geo_nav::answer_view_requests
                    .before(systems::private_chat::drive_private_chats),
                // A chat's "+ → Meet up": where to propose, halfway along the
                // walk when the other person is sharing (`systems::meetup`).
                systems::meetup::answer_meetup_requests,
                systems::geo_nav::drive_geocode,
                systems::navigation::drive_route_geocoding,
                // A route to a person moves with their shared location. After
                // the request that may have just started following them.
                systems::navigation::follow_shared_location.after(systems::geo_nav::drive_geocode),
                // A walk to a building ends at its door. After the request that
                // set the destination, so it is moved before the solver sees it.
                systems::navigation::snap_walk_to_door.after(systems::geo_nav::drive_geocode),
            )
                .in_set(ClientPhase::Input),
        )
        .add_systems(
            Update,
            (
                // Globe-descent re-anchor → teleport (must precede the teleport).
                systems::geo_nav::apply_camera_reanchor.before(systems::geo_nav::apply_teleport),
                // Restore feeds pending_teleport, so it must run before the
                // teleport applies; teleports move the camera, so apply before
                // camera_controls reads it this frame.
                systems::geo_nav::apply_teleport
                    .after(systems::geo_nav::drive_geocode)
                    .after(systems::user_store::restore_start_location)
                    // The avatar has to be *in* the BigSpace before it can be
                    // teleported within it: `attach_avatar_to_bigspace` inserts the
                    // `GridCell` that `apply_teleport`'s avatar query filters on,
                    // and without this edge the startup restore ran first, missed
                    // the query and moved only the camera. Bevy inserts the sync
                    // point that makes the deferred insert visible because this
                    // ordering is explicit.
                    .after(crate::systems::local_avatar::attach_avatar_to_bigspace)
                    .before(systems::free_camera::camera_controls),
            )
                .in_set(ClientPhase::Anchor),
        )
        .add_systems(
            Update,
            (
                systems::transit_live::update_world_clock,
                systems::transit_live::update_local_timezone
                    .after(systems::transit_live::update_world_clock),
                systems::transit_live::poll_transit_live
                    .after(systems::data_layers::update_data_layers)
                    .after(systems::transit_live::update_world_clock),
                // Live moving vehicles (ADR-026): fold new GPS fixes after the
                // data layer has (re)established its anchor and the clock ticked.
                systems::transit_vehicles::poll_transit_vehicles
                    .after(systems::data_layers::update_data_layers)
                    .after(systems::transit_live::update_world_clock),
            )
                .in_set(ClientPhase::Stream),
        )
        .add_systems(
            Update,
            (
                // Measures the drive (position, speed, course, progress) before
                // anything reads it. After the panel, so the button press that
                // starts driving is acted on in the same frame it was captured.
                // …and before the solver, so a reroute it asks for on leaving the
                // route is picked up in the same frame rather than a frame late.
                systems::driver::drive_driver_mode
                    .after(systems::navigation::render_route_sheet)
                    .after(avatar::handle_avatar_movement)
                    .before(systems::navigation::drive_route_solver),
                // Device tracking (`systems::locate`) — the locate button's own
                // mode, and the smaller sibling of the drive above. After the
                // drive tick, which is what declares driver mode active and so
                // decides which of the two owns the avatar this frame; before the
                // camera, which is what consumes the heading it writes.
                systems::locate::track_device_pose
                    .after(systems::driver::drive_driver_mode)
                    .after(avatar::handle_avatar_movement)
                    .before(systems::free_camera::camera_controls),
                // The camera mode on the locate button: after the button's own
                // tick, so the fix and heading it records are this frame's.
                systems::high_precision::drive_high_precision
                    .after(systems::locate::track_device_pose),
                // Walking a route in the world (`systems::walk_guide`): measured
                // from where the avatar just moved to, and before the solver for
                // the same reason the drive tick is — a reroute it asks for is
                // picked up the same frame. Its steering is read by next frame's
                // movement, one frame of latency on a hands-off walk.
                systems::walk_guide::drive_walk_guide
                    .after(systems::driver::drive_driver_mode)
                    .after(avatar::handle_avatar_movement)
                    .before(systems::navigation::drive_route_solver),
                // Arriving at a meet-up ends its location share: after both
                // arrival latches have been set this frame.
                systems::meetup::finish_on_arrival
                    .after(systems::walk_guide::drive_walk_guide)
                    .after(systems::driver::drive_driver_mode),
            )
                .in_set(ClientPhase::Move),
        )
        .add_systems(
            Update,
            // Writes the chase rig into `CameraState`, so it must land before the
            // camera consumes it — this is the whole mechanism by which driver
            // mode holds the view still.
            systems::driver::steady_driver_camera
                .after(systems::driver::drive_driver_mode)
                .before(systems::free_camera::camera_controls)
                .in_set(ClientPhase::Camera),
        )
        .add_systems(
            Update,
            (
                // Route planner (ADR-037).
                systems::navigation::drive_route_solver
                    .after(systems::navigation::render_route_sheet),
                // After the camera has moved, so the cursor ray matches the frame
                // the user actually clicked on.
                systems::navigation::handle_route_map_pick
                    .after(systems::free_camera::camera_controls),
                systems::navigation::update_route_visuals
                    .after(systems::navigation::drive_route_solver),
                // After the avatar has been moved (and after the camera, so the
                // arrow is placed against the same frame the view was built from).
                systems::driver::update_driver_arrow
                    .after(systems::driver::drive_driver_mode)
                    .after(systems::free_camera::camera_controls),
                // What lies ahead on the route (the rail's markers, the speed
                // limit). After the drive tick, which is what turns driver mode on,
                // and before the chrome that reads it.
                systems::route_ahead::drive_route_ahead.after(systems::driver::drive_driver_mode),
                systems::driver::render_driver_hud
                    .after(systems::ui::render_main_menu)
                    .after(systems::route_ahead::drive_route_ahead),
                systems::walk_guide::render_walk_guide.after(systems::ui::render_main_menu),
                // Transit in-world rendering. Both run after the camera has moved
                // this frame: boards billboard toward it, vehicles interpolate
                // relative to it.
                systems::transit_live::update_transit_boards
                    .after(systems::transit_live::poll_transit_live)
                    .after(systems::free_camera::camera_controls),
                systems::transit_vehicles::update_transit_vehicles
                    .after(systems::transit_vehicles::poll_transit_vehicles)
                    .after(systems::free_camera::camera_controls),
                // Ambient traffic: one transform write per car per frame, and no
                // asset touched after startup (`systems::traffic`).
                systems::traffic::drive_traffic,
            )
                .in_set(ClientPhase::Present),
        );

        // Turn cues. With the rest of the soundscape, behind the `audio` feature,
        // and after the drive tick because a cue is a function of the distance it
        // just computed — it has to see this frame's band, not last frame's.
        //
        // This is the one part of navigation you can use without looking at the
        // screen, which on a phone in a car is the only way it gets used at all.
        #[cfg(feature = "audio")]
        app.add_systems(
            Update,
            systems::driver::play_driver_cues
                .after(systems::driver::drive_driver_mode)
                .in_set(ClientPhase::Present),
        );
    }
}
