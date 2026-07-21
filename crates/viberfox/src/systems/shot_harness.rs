//! Scripted screenshot harness (`--shot`): fly the camera to one or more
//! lat/lng/altitude/orientation set-ups, wait for the world to stream in, and
//! save each frame as a PNG — then exit.
//!
//! This exists so a rendering question ("is the globe actually on screen at
//! 180 km?") can be answered by running the app from a shell, instead of flying
//! there by hand and grabbing the desktop. Each capture also logs a `shot state`
//! line (altitude, tier, near/far, globe entity/visible/rendered counts) so a
//! black image can be told apart from a black *scene*.
//!
//! ```bash
//! # one shot
//! cargo run -p viberfox --profile dev-bevy -- \
//!     --shot /tmp/shot.png --at 53.2194,6.5665,180000 --look=-90,0
//!
//! # a sweep: one app launch, one PNG per --at (suffixed with the altitude)
//! cargo run -p viberfox --profile dev-bevy -- --shot /tmp/sweep.png \
//!     --at 53.2194,6.5665,60000 --at 53.2194,6.5665,180000 --look=-90,0
//! ```
//!
//! Three things make it cheap and side-effect free:
//!   * **Headless.** No window is created at all — `main` drops `WinitPlugin` and
//!     drives the frames with `ScheduleRunnerPlugin`, and the camera renders into
//!     an offscreen image. Nothing appears on the desktop, no compositor can
//!     occlude (and blacken) the captured frame, and the run needs no display
//!     server. `resources::HeadlessSurface` stands in for the missing `Window`
//!     wherever one is needed (egui layout, screen-space LOD).
//!   * **Settle-on-idle.** Rather than a fixed sleep, each capture waits until
//!     the tile streams go quiet ([`MapStream::streaming_busy`] /
//!     [`GlobeState::streaming_busy`]) for `--settle` seconds, capped by
//!     `--wait`. Cached tiles settle in about a second.
//!   * **No user-store writes.** The interactive session's saved location is
//!     neither restored nor overwritten by a scripted run.
//!
//! The egui overlay is part of the capture (it carries the HUD's altitude and
//! coordinates, which is what makes a shot self-documenting); pass `--hide-ui`
//! for chrome-free images.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use big_space::prelude::GridCell;

use crate::resources::{CameraState, ScreenshotState};
use crate::systems::free_camera::FreeCamera;
use crate::systems::geo_nav::{CameraPose, GeoNavState, Teleport};
use crate::systems::globe::{GlobeMesh, GlobeState};
use crate::systems::map_stream::MapStream;

/// One scripted viewpoint.
#[derive(Clone)]
pub struct ShotSpec {
    pub lat: f64,
    pub lng: f64,
    /// Metres above the ground at (lat, lng).
    pub alt: f64,
    /// Degrees: −90 = straight down, 0 = horizon.
    pub pitch_deg: f32,
    /// Degrees: 0 = north, clockwise.
    pub yaw_deg: f32,
}

/// The scripted run, parsed from the CLI. Present as a resource only when
/// `--shot` was passed; its absence is what other systems test to know the app
/// is in normal interactive mode.
#[derive(Resource, Clone)]
pub struct ShotHarness {
    /// Base output path; with more than one shot the altitude is appended
    /// (`/tmp/s.png` → `/tmp/s_180000m.png`).
    pub path: PathBuf,
    pub shots: Vec<ShotSpec>,
    /// Offscreen render size.
    pub size: UVec2,
    /// Seconds of quiet streaming required before capturing.
    pub settle: f32,
    /// Hard cap on the wait per shot.
    pub wait: f32,
    /// Leave the egui overlay out of the capture.
    pub hide_ui: bool,
    /// "free" (exact placement) or "earth" (the orbit camera).
    pub mode: String,
}

impl ShotHarness {
    /// Build from the raw CLI strings: `at` is one or more "lat,lng,alt", `look`
    /// zero or more "pitch,yaw" (paired by index; the last one carries over).
    #[allow(clippy::too_many_arguments)]
    pub fn from_cli(
        path: PathBuf,
        at: &[String],
        look: &[String],
        size: &str,
        settle: f32,
        wait: f32,
        hide_ui: bool,
        mode: String,
    ) -> Result<Self, String> {
        fn nums(s: &str, want: usize, what: &str) -> Result<Vec<f64>, String> {
            let v: Vec<f64> = s
                .split(',')
                .map(|p| p.trim().parse::<f64>().map_err(|e| format!("{s:?}: {e}")))
                .collect::<Result<_, _>>()?;
            (v.len() == want)
                .then_some(v)
                .ok_or_else(|| format!("{what} wants {want} comma-separated numbers, got {s:?}"))
        }

        let mut shots = Vec::new();
        for (i, at) in at.iter().enumerate() {
            let p = nums(at, 3, "--at")?;
            // Fewer --look than --at: the last one applies to the rest.
            let angles = look
                .get(i)
                .or_else(|| look.last())
                .map(|l| nums(l, 2, "--look"))
                .transpose()?
                .unwrap_or_else(|| vec![-90.0, 0.0]);
            shots.push(ShotSpec {
                lat: p[0],
                lng: p[1],
                alt: p[2],
                pitch_deg: angles[0] as f32,
                yaw_deg: angles[1] as f32,
            });
        }
        if shots.is_empty() {
            return Err("--shot needs at least one --at lat,lng,alt".into());
        }
        let size = parse_size(size).ok_or_else(|| format!("--size wants WxH, got {size:?}"))?;
        Ok(Self { path, shots, size, settle, wait, hide_ui, mode })
    }

    /// Output path for shot `i` (bare `--shot` path for a single capture).
    fn path_for(&self, i: usize) -> PathBuf {
        if self.shots.len() == 1 {
            return self.path.clone();
        }
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned());
        let ext = self.path.extension().map(|s| s.to_string_lossy().into_owned());
        let name = format!(
            "{}_{}m.{}",
            stem.as_deref().unwrap_or("shot"),
            self.shots[i].alt as i64,
            ext.as_deref().unwrap_or("png"),
        );
        self.path.with_file_name(name)
    }
}

/// Parse a `--size` string ("1280x720"). Also read in `main` to size the window
/// to the offscreen target — egui scissors against the window, so the two must
/// agree or wgpu validation kills the run (see the `shot_size` comment there).
pub fn parse_size(size: &str) -> Option<UVec2> {
    let (w, h) = size.split_once(['x', 'X'])?;
    Some(UVec2::new(w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// The offscreen colour target the camera renders into.
#[derive(Resource)]
pub struct ShotTarget(pub Handle<Image>);

/// Where the scripted run has got to.
#[derive(Resource, Default)]
pub struct ShotProgress {
    /// Index of the shot being set up / captured.
    index: usize,
    /// The teleport for this shot has been issued.
    placed: bool,
    /// Seconds since placement.
    elapsed: f32,
    /// Seconds the tile streams have been quiet.
    quiet: f32,
    /// Frames and wall-clock seconds since placement, for the mean frame time
    /// reported with each capture (how the space tier's render cost is A/B'd).
    frames: u32,
    frame_time: f32,
    /// The capture frame has been requested (waiting on the GPU readback).
    fired: bool,
    /// Flipped by the capture observer once the PNG is on disk.
    saved: Arc<AtomicBool>,
    /// Frames left before exiting (drains after the last save).
    exit_in: Option<u32>,
}

/// True while a scripted capture is configured — the run condition the user
/// store uses to keep out of the way.
pub fn harness_active(harness: Option<Res<ShotHarness>>) -> bool {
    harness.is_some()
}

/// Point the camera at an offscreen image of the requested size, so the run
/// renders (and captures) without a visible window. Runs at `Startup` after
/// `free_camera::setup_camera` has spawned the camera.
pub fn setup_offscreen_target(
    mut commands: Commands,
    harness: Res<ShotHarness>,
    mut images: ResMut<Assets<Image>>,
    // `RenderTarget` is its own component in Bevy 0.19 (required by `Camera`).
    mut camera: Query<&mut RenderTarget, With<FreeCamera>>,
) {
    let mut image = Image::new_fill(
        Extent3d {
            width: harness.size.x,
            height: harness.size.y,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    // RENDER_ATTACHMENT to draw into it, COPY_SRC so the screenshot readback can
    // pull it back to the CPU.
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_SRC
        | TextureUsages::RENDER_ATTACHMENT;
    let handle = images.add(image);
    if let Ok(mut target) = camera.single_mut() {
        *target = RenderTarget::Image(handle.clone().into());
    }
    commands.insert_resource(ShotTarget(handle));
}

/// Put the camera on the current shot's target as soon as the map anchor
/// exists, reusing the normal teleport path (so a scripted run lands exactly
/// where a bookmark jump would).
pub fn place_camera(
    harness: Res<ShotHarness>,
    mut progress: ResMut<ShotProgress>,
    stream: Res<MapStream>,
    mut nav: ResMut<GeoNavState>,
) {
    if progress.placed || stream.geo_anchor().is_none() {
        return;
    }
    let Some(shot) = harness.shots.get(progress.index) else {
        return;
    };
    progress.placed = true;
    progress.elapsed = 0.0;
    progress.quiet = 0.0;
    progress.frames = 0;
    progress.frame_time = 0.0;
    let pose = CameraPose {
        mode: harness.mode.clone(),
        // free_camera_rotation is (pitch, yaw), in radians.
        free_rotation: [shot.pitch_deg.to_radians(), shot.yaw_deg.to_radians()],
        azimuth: 0.0,
        pitch: 0.5,
        distance: 5.0,
        // Earth mode: apply_teleport recomputes the orbit distance from
        // `altitude`, so this only needs to be non-zero.
        orbit_distance: shot.alt,
        orbit_yaw: shot.yaw_deg.to_radians(),
        orbit_pitch: 1.55,
    };
    nav.pending_teleport = Some(Teleport {
        lat: shot.lat,
        lng: shot.lng,
        altitude: Some(shot.alt),
        pose: Some(pose),
    });
    tracing::info!(
        shot = progress.index + 1,
        of = harness.shots.len(),
        lat = shot.lat,
        lng = shot.lng,
        alt = shot.alt,
        mode = %harness.mode,
        "shot harness: camera placed"
    );
}

/// `--shot-mode earth` above the space threshold: put the orbit camera straight
/// into its space tier over the target. The ordinary teleport path caps the
/// surface orbit at 250 km (`ORBIT_MAX_DISTANCE_SURFACE`) and lets the handoff
/// promote it from there, which is right for a user jump but leaves a scripted
/// "show me the globe from 15,000 km" run parked just above the atmosphere.
/// Runs after the teleport so it has the last word.
pub fn place_space_orbit(
    harness: Res<ShotHarness>,
    progress: Res<ShotProgress>,
    globe: Res<GlobeState>,
    mut cam_state: ResMut<CameraState>,
    mut done: Local<usize>,
) {
    let Some(shot) = harness.shots.get(progress.index) else {
        return;
    };
    // Once per shot, and only for an earth-mode run that wants to be in space.
    if !progress.placed || *done > progress.index || harness.mode != "earth" || !globe.ready {
        return;
    }
    *done = progress.index + 1;
    if shot.alt <= 60_000.0 {
        return;
    }
    cam_state.orbit_space = true;
    cam_state.orbit_initialized = true;
    cam_state.orbit_dir = globe.direction_local(shot.lat, shot.lng).as_dvec3();
    cam_state.orbit_distance = globe.radius as f64 + shot.alt;
    cam_state.orbit_distance_target = cam_state.orbit_distance;
    // Heading 0: the globe comes up north-up, like every other space arrival.
    cam_state.orbit_heading = 0.0;
}

/// Wait for the view to settle, capture, then move to the next shot (or exit).
#[allow(clippy::too_many_arguments)]
pub fn drive_capture(
    mut commands: Commands,
    time: Res<Time>,
    harness: Res<ShotHarness>,
    target: Option<Res<ShotTarget>>,
    mut progress: ResMut<ShotProgress>,
    mut shot_state: ResMut<ScreenshotState>,
    mut exit: MessageWriter<AppExit>,
    globe: Res<GlobeState>,
    stream: Res<MapStream>,
    cam_state: Res<CameraState>,
    camera: Query<(&GridCell, &Transform, &Projection), With<FreeCamera>>,
    globe_meshes: Query<(&bevy::camera::visibility::ViewVisibility, &Visibility), With<GlobeMesh>>,
) {
    if let Some(frames) = progress.exit_in.as_mut() {
        *frames = frames.saturating_sub(1);
        if *frames == 0 {
            exit.write(AppExit::Success);
        }
        return;
    }
    // Capture landed: on to the next viewpoint, or done.
    if progress.saved.swap(false, Ordering::Relaxed) {
        progress.index += 1;
        if progress.index >= harness.shots.len() {
            progress.exit_in = Some(2);
        } else {
            progress.placed = false;
            progress.fired = false;
        }
        return;
    }
    if !progress.placed {
        return;
    }
    progress.elapsed += time.delta_secs();
    // Only count settled frames: the first second after a teleport is dominated
    // by tile decode/upload, not by the steady-state render cost being measured.
    if progress.quiet > 0.0 {
        progress.frames += 1;
        progress.frame_time += time.delta_secs();
    }
    if progress.fired {
        // The readback normally lands within a frame or two; don't hang forever.
        if progress.elapsed > harness.wait + 20.0 {
            tracing::error!("shot harness: capture never completed");
            exit.write(AppExit::error());
        }
        return;
    }

    // Settle: capture once both tile streams have been quiet for `--settle`
    // seconds, or when `--wait` runs out (whichever comes first).
    if globe.streaming_busy() || stream.streaming_busy() {
        progress.quiet = 0.0;
    } else {
        progress.quiet += time.delta_secs();
    }
    if progress.quiet < harness.settle && progress.elapsed < harness.wait {
        return;
    }

    // The numbers behind the image, so a black frame can be diagnosed without
    // re-running: a black *scene* still reports visible, rendered globe meshes.
    if let Ok((cell, tf, projection)) = camera.single() {
        let (near, far) = match projection {
            Projection::Perspective(p) => (p.near, p.far),
            _ => (0.0, 0.0),
        };
        tracing::info!(
            shot = progress.index + 1,
            settled_in = format!("{:.1}s", progress.elapsed),
            frame_ms = format!(
                "{:.1}",
                1000.0 * progress.frame_time / progress.frames.max(1) as f32
            ),
            alt_m = globe.altitude_m(cell, tf) as i64,
            in_space = globe.in_space(cell, tf),
            orbit_space = cam_state.orbit_space,
            near_m = near as i64,
            far_m = far as i64,
            globe_entities = globe_meshes.iter().count(),
            globe_visible = globe_meshes.iter().filter(|(_, v)| **v == Visibility::Visible).count(),
            globe_rendered = globe_meshes.iter().filter(|(vv, _)| vv.get()).count(),
            "shot state"
        );
    }

    // `--hide-ui`: drop this frame's egui geometry (ui::suppress_egui_for_screenshot
    // reads `hide_ui` in PostUpdate; ui::drive_screenshots clears it next frame).
    shot_state.hide_ui = harness.hide_ui;
    progress.fired = true;
    let saved = progress.saved.clone();
    let path = harness.path_for(progress.index);
    let screenshot = match target.as_ref() {
        Some(t) => Screenshot::image(t.0.clone()),
        None => Screenshot::primary_window(),
    };
    commands
        .spawn(screenshot)
        .observe(move |captured: On<ScreenshotCaptured>| {
            let (saved, path) = (saved.clone(), path.clone());
            match captured.image.clone().try_into_dynamic() {
                // Alpha carries HDR brightness rather than transparency here
                // (as in the README capture path), so drop it.
                Ok(img) => match img.to_rgb8().save(&path) {
                    Ok(()) => {
                        tracing::info!(path = %path.display(), "shot harness: saved");
                        saved.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::error!(path = %path.display(), error = %e, "shot save failed")
                    }
                },
                Err(e) => tracing::error!(error = %e, "shot decode failed"),
            }
        });
}
