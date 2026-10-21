//! Registration for `systems::dev`: the developer panel, frame pacing and the
//! diagnostics that run in every build ([`DevPlugin`]), and the three things
//! that exist only on some launches — the camera operator ([`ShotHarnessPlugin`]),
//! the performance suite ([`PerfPlugin`]) and the editor's way in
//! ([`McpPlugin`]). Those three are added by `run_client` only when the command
//! line asks for them, carrying what it resolved as fields. The phase each
//! `Update` system runs in is `crate::phase`'s; the edges beside them are the
//! ones `lib.rs` carried before the split.

use bevy::prelude::*;

use crate::phase::ClientPhase;
use crate::systems;
use crate::systems::{avatar, frame_pacing};

pub struct DevPlugin;

impl Plugin for DevPlugin {
    fn build(&self, app: &mut App) {
        // Which screen of the developer panel is showing (`systems::dev_panel`) —
        // a resource so `--ui profiler` can name one.
        app.init_resource::<systems::dev_panel::DevTools>()
            .init_resource::<systems::upload_meter::UploadMeter>()
            .init_resource::<systems::diagnostics::DiagnosticsRequest>()
            .init_resource::<frame_pacing::PresentationRate>();

        // The moving world's clock (`systems::anim_clock`): the virtual clock in
        // every run but a `--video-fps` movie, which `ShotHarnessPlugin` swaps in.
        // Its render-world half hands the shaders that clock as `globals.time`.
        app.init_resource::<systems::anim_clock::AnimClock>()
            .add_plugins(bevy::render::extract_resource::ExtractResourcePlugin::<
                systems::anim_clock::AnimClock,
            >::default())
            .add_systems(
                First,
                systems::anim_clock::tick_anim_clock.after(bevy::time::TimeSystems),
            );
        // Where each frame's time went (`systems::frame_split`), on every platform:
        // a timestamp at each end of the main world and of the render world.
        app.add_systems(
            First,
            systems::frame_split::begin_main.before(bevy::time::TimeSystems),
        )
        .add_systems(Last, systems::frame_split::end_main)
        // How evenly a drag moves the camera (`systems::camera_pacing`).
        .init_resource::<systems::camera_pacing::AppliedDrag>()
        .add_systems(Last, systems::camera_pacing::measure_camera_pacing);
        if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
            render_app
                .add_systems(
                    bevy::render::ExtractSchedule,
                    systems::frame_split::begin_render,
                )
                .add_systems(
                    bevy::render::Render,
                    systems::frame_split::end_render
                        .in_set(bevy::render::RenderSystems::PostCleanup),
                );
        }
        if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
            render_app.add_systems(
                bevy::render::Render,
                systems::anim_clock::shader_time_from_anim_clock
                    .after(bevy::render::RenderSystems::ExtractCommands)
                    .before(bevy::render::RenderSystems::PrepareResources),
            );
        }

        app.add_systems(
            Update,
            (
                systems::dev_panel::toggle_dev_panel_shortcut,
                systems::dev_panel::render_dev_panel,
            )
                .in_set(ClientPhase::Input),
        )
        .add_systems(
            Update,
            (
                // Per-window OS icon via winit (Windows + Linux/X11; a no-op on
                // Wayland and macOS, which use the desktop file / .app bundle
                // respectively). See `systems::window_icon`.
                systems::window_icon::set_window_icon,
                frame_pacing::frame_time_watchdog,
                frame_pacing::frame_stats,
                frame_pacing::apply_screen_wakelock,
            )
                .in_set(ClientPhase::Present),
        );

        // In `Last`, not `Update`, and the placement is load-bearing — see
        // `measure_uploads` for both halves of why (a mesh's data is gone after
        // extraction, and this is where the frame's assets are all present and
        // all still readable). Registered on every tier, unlike `log_render_cost`:
        // it holds two message readers and two `Res<Assets<_>>`, matches no
        // archetypes at all, and the phone is the platform whose bursts it names.
        // (`load_control::observe`, the meter's other consumer, is `map::plugin`'s.)
        app.add_systems(Last, systems::upload_meter::measure_uploads)
            // Same schedule and the same reason as `log_avatar_draw_counts`: the
            // report quotes `ViewVisibility`, which `PostUpdate` writes, so from
            // `Update` it would describe the frame before the tap.
            .add_systems(Last, systems::diagnostics::share_diagnostics)
            // Four systems, one chain, and each position is load-bearing.
            //
            // `present_rate` runs **first**, because both of the next two consume
            // what it decides: it is the single resolution of "what rate do we
            // want", handed to the compositor on Android and slept to by the
            // limiter elsewhere. Resolved in `Last` rather than `First` so the idle
            // test can read this frame's finished work — the bytes uploaded and the
            // camera's settled transform — instead of the previous frame's.
            //
            // `report_frame_to_scheduler` runs **before** the limiter: it tells
            // ADPF how long the frame took, and the limiter *sleeps* to hit a
            // deadline, so measuring after it would report the sleep as work and
            // describe a process that is always exactly on time however slow it
            // really is.
            //
            // `watchdog_beat` runs **after** it, for the mirror-image reason: the
            // heartbeat has to mean "a whole frame completed", so anything that can
            // wedge the schedule — a system, a render extract, the limiter's own
            // sleep — is inside the interval the watchdog measures. Beating in
            // `First` would say the loop is alive at the moment it enters the frame
            // it is about to hang in, which is the one frame that matters.
            .add_systems(
                Last,
                (
                    frame_pacing::present_rate,
                    frame_pacing::report_frame_to_scheduler,
                    // Not under the shot harness. The limiter sleeps to a deadline,
                    // so with it running `ShotMetrics::frame_ms` can never read
                    // below the tier's cap however cheap the frame got — an
                    // optimisation worth measuring is exactly one the limiter would
                    // hide. A scripted capture has no display to pace to and no
                    // eyes on it anyway.
                    frame_pacing::frame_limiter.run_if(not(systems::shot_harness::harness_active)),
                    frame_pacing::watchdog_beat,
                )
                    .chain(),
            );
    }
}

/// **The camera operator.** One state machine places a camera, waits for every
/// streamer to go quiet, quiesces the CPU pool so the GPU readback is not queued
/// behind a rasterise backlog, fires the shutter and measures what it
/// photographed. Three things drive it and none of them owns a copy: `--shot`
/// and `--shots` hand it a list and quit at the end, `--perf` measures a window
/// after each shutter, and `--mcp-listen` hands it one viewpoint at a time while
/// the app runs.
pub struct ShotHarnessPlugin {
    pub harness: systems::shot_harness::ShotHarness,
}

impl Plugin for ShotHarnessPlugin {
    fn build(&self, app: &mut App) {
        // Fed at runtime: no `--ui` list to apply, and the holds must let go of
        // the camera between calls.
        let live = self.harness.live;
        let windowless = !self.harness.windowed;
        // No window at all: the camera renders into an offscreen image and
        // `HeadlessSurface` stands in for the `Window` egui lays out against.
        // A windowed MCP session has a real one and takes neither.
        if windowless {
            app.insert_resource(crate::resources::HeadlessSurface {
                size: self.harness.size,
                scale: 1.0,
            })
            .add_systems(
                Startup,
                systems::shot_harness::setup_offscreen_target
                    .after(systems::free_camera::setup_camera),
            );
        }
        // A movie (`--video-fps`) freezes the moving world between frames; the
        // capture steps it (`drive_capture`). Replaces `DevPlugin`'s plain clock.
        if let Some(fps) = self.harness.video_fps {
            app.insert_resource(systems::anim_clock::AnimClock::video(fps));
        }
        app.insert_resource(self.harness.clone())
            .init_resource::<systems::shot_harness::ShotProgress>()
            .add_systems(
                Update,
                (
                    // Anywhere in Update is early enough: it only has to beat
                    // PostUpdate's `end_egui_frame`, which is what would otherwise
                    // reuse an older frame's tessellation for the capture.
                    //
                    // Gated, because in a live run this is a cost paid every frame
                    // for a capture that may be minutes away — and between calls
                    // the ordinary skip is what the client wants.
                    systems::shot_harness::keep_egui_fresh
                        .run_if(systems::shot_harness::shot_in_progress)
                        .in_set(ClientPhase::Input),
                    systems::shot_harness::place_camera
                        .before(systems::geo_nav::apply_teleport)
                        .in_set(ClientPhase::Input),
                    systems::shot_harness::place_space_orbit
                        .after(systems::geo_nav::apply_teleport)
                        .before(systems::free_camera::camera_controls)
                        .in_set(ClientPhase::Anchor),
                    // Hold the scripted camera mode against the toggle that would
                    // drop an offline Follow to Fly, and before the movement system
                    // that reads the mode. **Gated**: a mode held every frame by an
                    // idle camera operator is a mode the person at the keyboard
                    // cannot change.
                    systems::shot_harness::hold_scripted_camera_mode
                        .after(systems::free_camera::camera_mode_toggle)
                        .before(avatar::handle_avatar_movement)
                        .run_if(systems::shot_harness::shot_in_progress)
                        .in_set(ClientPhase::Input),
                    // `--look-at`: aim at a place rather than at an angle. After
                    // the teleport has put the camera somewhere and before the
                    // camera system reads the rotation, and every frame, because
                    // the terrain under both ends lands a second after the anchor.
                    // Gated for the reason the mode hold is: an idle camera
                    // operator's camera belongs to the person at the keyboard.
                    systems::shot_harness::aim_scripted_camera
                        .after(systems::geo_nav::apply_teleport)
                        .before(systems::free_camera::camera_controls)
                        .run_if(systems::shot_harness::shot_in_progress)
                        .in_set(ClientPhase::Anchor),
                    // A preset's roll, lens and ground-relative height, held for
                    // the reasons and with the gate the aim has.
                    systems::shot_harness::pose_scripted_camera
                        .after(systems::geo_nav::apply_teleport)
                        .before(systems::free_camera::camera_controls)
                        .run_if(systems::shot_harness::shot_in_progress)
                        .in_set(ClientPhase::Anchor),
                    // Ungated, and it has to be: it owns the advance past the last
                    // viewpoint — which is the edge that parks a live run and the
                    // one that ends a scripted one.
                    //
                    // After drive_screenshots so the `hide_ui` it clears each frame
                    // isn't cleared again before PostUpdate's egui suppression
                    // reads it.
                    systems::shot_harness::drive_capture
                        .after(systems::free_camera::camera_controls)
                        .after(systems::ui::drive_screenshots)
                        // The settle test reads the registry the reporters in
                        // this set fill (`systems::stream_watch`).
                        .after(systems::stream_watch::StreamReport)
                        .in_set(ClientPhase::Present),
                ),
            );
        // The `--ui` family, which only a *scripted* run has: a live harness is
        // handed viewpoints, not presets, and `open_scripted_ui` keyed on the shot
        // index would close every panel the user had open each time an editor
        // took a picture.
        if !live {
            app.add_systems(
                Update,
                (
                    systems::shot_harness::open_scripted_ui.in_set(ClientPhase::Input),
                    systems::shot_harness::hold_scripted_clip.in_set(ClientPhase::Input),
                    // After the camera is placed, so the demo speaker `--ui chat`
                    // spawns stands where this shot's camera is actually looking.
                    systems::shot_harness::hold_scripted_chat
                        .after(systems::shot_harness::open_scripted_ui)
                        .in_set(ClientPhase::Input),
                    // After the camera is placed, for the same reason the chat demo
                    // is: the seeded prims stand where this shot is actually looking.
                    systems::shot_harness::hold_scripted_fire
                        .after(systems::shot_harness::open_scripted_ui)
                        .in_set(ClientPhase::Input),
                    // The same, for the selected prim the editing handles stand on.
                    systems::shot_harness::hold_scripted_gizmo
                        .after(systems::shot_harness::open_scripted_ui)
                        .in_set(ClientPhase::Input),
                    // After the teleport for the reason its doc gives — the teleport
                    // clears the seat — and before the camera, so the shift it
                    // applies is framed on the same frame it is written.
                    systems::shot_harness::seat_scripted_avatar
                        .after(systems::geo_nav::apply_teleport)
                        .after(systems::shot_harness::hold_scripted_camera_mode)
                        .before(systems::free_camera::camera_controls)
                        .in_set(ClientPhase::Anchor),
                    // `--ui building`: select the building nearest the avatar, since
                    // a headless run has no cursor to click one with. After the
                    // teleport, which is what decides where "nearest" is measured
                    // from; the glass it asks for is built by
                    // `update_building_shells` on the next frame, which a settling
                    // capture has hundreds of.
                    systems::shot_harness::select_scripted_building
                        .after(systems::geo_nav::apply_teleport)
                        .in_set(ClientPhase::Anchor),
                    // `--ui locate`: a fix with no provider, so the device arrow can
                    // be photographed at all.
                    //
                    // **After** the tracking system, not before. That system reads
                    // the live watches every frame and writes what it finds — which
                    // in a scripted run is nothing, since no provider exists — so a
                    // seed seen first is a seed wiped before the overlay reads it.
                    // Running last is the whole reason `--ui locate` produces a
                    // picture rather than an empty map.
                    //
                    // …and **before the camera**, which is the other half of the
                    // same argument: with `--ui firstperson` the seeded attitude
                    // is what aims the view, and a rotation written after
                    // `camera_controls` has read it is one frame of lag on a
                    // capture that only ever takes one frame.
                    systems::shot_harness::track_scripted_device
                        .after(systems::geo_nav::apply_teleport)
                        .after(systems::locate::track_device_pose)
                        .before(systems::free_camera::camera_controls)
                        .before(systems::locate::render_locate_arrow)
                        .in_set(ClientPhase::Move),
                    // `--ui firstperson`: zoom the follow camera into the avatar's
                    // head. Before the device seeding above, which asks whether it
                    // is on, and before the camera that acts on both.
                    systems::shot_harness::zoom_scripted_first_person
                        .before(systems::shot_harness::track_scripted_device)
                        .before(systems::free_camera::camera_controls)
                        .in_set(ClientPhase::Input),
                ),
            );
        }
    }
}

/// The performance suite rides the harness: same teleport, same settle test,
/// same shutter. All it adds is when to measure.
pub struct PerfPlugin {
    pub run: systems::perf::PerfRun,
}

impl Plugin for PerfPlugin {
    fn build(&self, app: &mut App) {
        // The per-pass GPU timings, which the suite's report is half built on.
        // They are behind `CARTO_GPU_TIMINGS` for every other launch (see the
        // plugin block in `run_client`) because the readback can take a driver
        // down at startup — but a `cargo perf` run is a measurement run by
        // definition, so it opts itself in. Guarded, because the switch may
        // already have added it and bevy answers a duplicate plugin with a panic.
        //
        // Not on Metal, where it is not a column that stays empty but a run that
        // measures nothing: wgpu's Metal backend fails to allocate the counter
        // sample buffer, reports it as `DeviceLost`, and `handle_render_error`
        // halts the renderer before the first frame — the suite then times an
        // empty frame at ~570 fps and the capture never completes (M5 Pro,
        // 2026-10-16). The cause is the build nodes' VM, not wgpu: its "Apple
        // Paravirtual device" lists no counter sets at all, so there is no
        // `timestamp` set to sample (probed 2026-10-21). Bare-metal Apple GPUs
        // have one, but no pipeline-statistics set, so `fill` stays absent on any
        // Mac. `CARTO_GPU_TIMINGS` still forces the plugin on.
        let metal = cfg!(target_vendor = "apple");
        if !metal && !app.is_plugin_added::<bevy::render::diagnostic::RenderDiagnosticsPlugin>() {
            app.add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin);
        }
        app.insert_resource(self.run.clone())
            .init_resource::<systems::perf::PerfProgress>()
            .init_resource::<systems::perf::MeshChurn>()
            .add_systems(
                Update,
                (
                    systems::perf::count_mesh_churn,
                    systems::perf::count_material_churn,
                    // **Before `drive_capture`**, which is load-bearing: the
                    // capture path consumes its `saved` flag with a swap, so a
                    // system that needs to see the capture land has to look first.
                    systems::perf::drive_perf_window,
                )
                    .chain()
                    // After it, so an `arrive` window reads what every streamer
                    // was left doing this frame.
                    .after(systems::stream_watch::StreamReport)
                    .before(systems::shot_harness::drive_capture)
                    .in_set(ClientPhase::Present),
            );
    }
}

/// **The editor's way in** (`systems::mcp`), on by default on the desktop and
/// switched from Settings ▸ General ▸ Editor access.
///
/// Everything is registered here; whether a socket is actually open is
/// `mcp::reconcile_listener`'s, once a frame's worth of change detection. The
/// thing this enables is an editor attaching to a client that is *already
/// running*, and a launch flag cannot serve that: the moment somebody wants it
/// is the moment something is on screen.
///
/// Added after the `UserStore` is in the world: the allow-list is read out of
/// it here.
#[cfg(mcp)]
pub struct McpPlugin {
    pub config: systems::mcp::Config,
    /// `--mcp-allow`, which pins the allow-list for this run.
    pub allow: Option<String>,
    /// `--mcp-private-only`: the agent speaks in private chats only.
    pub private_only: bool,
    /// What this launch signed in with, kept so an agent can sign back in after
    /// signing out (`mcp::agent`, `sign_in`).
    pub launch_sign_in: Option<systems::session::SessionRequest>,
}

#[cfg(mcp)]
impl Plugin for McpPlugin {
    fn build(&self, app: &mut App) {
        let cfg = &self.config;
        // "will", and the wording matters: the socket is opened a frame later by
        // `reconcile_listener`, which is the only thing that knows whether
        // another client already serves there. A line here saying "serving" is a
        // claim this code cannot make, and the run that proves it is the second
        // client on one socket, which would log "serving" and then "cannot
        // serve" three lines apart.
        if cfg.headless {
            tracing::info!(listen = %cfg.listen.display(), "mcp: headless, will serve");
        }
        let (tx, rx) = futures_channel::mpsc::unbounded();
        let headless = cfg.headless;
        // Whose chat the agent may read: the flag for this run, else what Settings
        // remembers, else nobody (`mcp::agent::Audience`).
        let mut audience = match &self.allow {
            Some(list) => systems::mcp::agent::Audience {
                pinned: true,
                ..systems::mcp::agent::Audience::parse(list)
            },
            None => systems::mcp::agent::Audience::parse(
                app.world()
                    .resource::<systems::user_store::UserStore>()
                    .data
                    .mcp_allow
                    .as_deref()
                    .unwrap_or(""),
            ),
        };
        audience.private_only = self.private_only;
        tracing::info!(can_hear = %audience.describe(), private_only = audience.private_only, "mcp: agent allow-list");
        app.insert_resource(audience)
            .insert_resource(match self.launch_sign_in.clone() {
                Some(request) => systems::mcp::agent::AgentState::signing_in_with(request),
                None => systems::mcp::agent::AgentState::default(),
            })
            .add_systems(
                Update,
                (
                    // After the pump, which is what drains the agent's requests
                    // off the server thread's queue.
                    systems::mcp::agent::serve.after(systems::mcp::pump),
                    // A walk is a travel wish, so it is aimed before the movement
                    // system reads it — and after the mode toggle, which would
                    // otherwise take back the Follow camera a walk needs.
                    systems::mcp::agent::steer
                        .after(systems::free_camera::camera_mode_toggle)
                        .before(avatar::handle_avatar_movement),
                )
                    .in_set(ClientPhase::Input),
            );
        app.insert_resource(cfg.clone())
            .insert_resource(systems::mcp::JobSender(tx))
            .init_resource::<systems::mcp::McpTrouble>()
            .insert_resource(systems::mcp::McpBridge::new(rx, cfg.listen.clone()))
            .add_systems(
                Update,
                (
                    // Opens or closes the socket to match the switch. Before the
                    // pump, so a listener enabled this frame can be answered from
                    // this frame.
                    systems::mcp::reconcile_listener.in_set(ClientPhase::Input),
                    // Before the camera operator, so a viewpoint that arrives
                    // this frame is placed this frame rather than waiting one.
                    systems::mcp::pump
                        .before(systems::shot_harness::place_camera)
                        .in_set(ClientPhase::Input),
                    // After it, so a capture that lands this frame is answered
                    // this frame. The two halves are ordered around
                    // `drive_capture` for the same reason and in opposite
                    // directions.
                    systems::mcp::land_captures
                        .after(systems::shot_harness::drive_capture)
                        .in_set(ClientPhase::Present),
                )
                    .chain(),
            );
        if headless {
            // Nobody sees a headless client between calls, so it slows right
            // down then (`mcp::pace_idle`).
            app.init_resource::<systems::mcp::FrameStart>()
                .add_systems(First, systems::mcp::mark_frame_start)
                .add_systems(Last, systems::mcp::pace_idle);
        } else {
            app.add_systems(
                Update,
                systems::mcp::overlay::render_mcp_activity
                    .after(systems::place_pins::render_place_pins)
                    .in_set(ClientPhase::Present),
            );
        }
    }
}
