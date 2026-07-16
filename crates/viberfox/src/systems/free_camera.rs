use bevy::math::{DQuat, DVec3};
use bevy::light::atmosphere::ScatteringMedium;
use bevy::camera::Exposure;
use bevy::light::{AmbientLight, Atmosphere, AtmosphereEnvironmentMapLight, NotShadowCaster};
use bevy::pbr::AtmosphereSettings;
use bevy::prelude::*;
use big_space::prelude::{BigSpaceCommands, FloatingOrigin, Grid, GridCell};

use crate::resources::{AvatarState, CameraState, CameraMode};
use crate::systems::egui_manager::EguiManager;
use crate::systems::gizmo::GizmoState;
use crate::systems::rendering::RegionMesh;

#[derive(Component)]
pub struct FreeCamera;

/// Builds the camera's [`DistanceFog`] for a dev-panel density in 0.0–1.0 and a
/// time of day. The falloff is exponential-squared so the near field stays clear
/// and geometry vanishes into a fog "wall"; the fog color follows the sky — bright
/// haze by day, warm-tinted through sunrise/sunset (fog is lit by the same
/// extinction-reddened sun the sky shows), near-black murk at night.
pub fn distance_fog(density: f32, hours: f32) -> DistanceFog {
    use crate::utils::sun;

    let day = sun::daylight(hours); // smooth through twilight, not a hard ramp
    let night_color = Vec3::new(0.015, 0.02, 0.035);
    // Slightly desaturated toward white so the fog wall meets the Mie-hazed sky.
    let day_color = Vec3::new(0.71, 0.75, 0.83);
    // Tint by the sun's transmittance color while it sits near the horizon, so
    // dawn/dusk haze glows warm with the sky instead of snapping grey→black.
    let t = sun::sun_transmittance(hours);
    let tint = t / t.max_element().max(1e-4);
    let warm = 0.65 * sun::horizon_proximity(hours);
    let c = night_color.lerp(day_color, day) * Vec3::ONE.lerp(tint, warm);

    // Map density to a visibility distance on an exponential curve:
    // 0.15 → ~7 km haze, 0.5 → ~1.2 km fog, 1.0 → ~90 m pea soup.
    let falloff = if density > 0.001 {
        FogFalloff::from_visibility_squared(15_000.0 * 0.006_f32.powf(density))
    } else {
        FogFalloff::Linear {
            start: f32::MAX,
            end: f32::MAX,
        }
    };

    DistanceFog {
        color: Color::srgb(c.x, c.y, c.z),
        // Sun-side glow in the fog, colored like the sun itself (white by day,
        // orange-red as it sets); fades out through early twilight.
        directional_light_color: Color::srgba(
            tint.x,
            tint.y,
            tint.z,
            0.55 * sun::smoothstep(-0.08, 0.02, sun::sun_direction(hours).y),
        ),
        directional_light_exponent: 30.0,
        falloff,
    }
}

/// Ground fog is full-strength up to this altitude (rooftop / low-flight haze).
const FOG_FADE_LOW: f32 = 800.0;
/// …and has thinned to nothing by this altitude, so an orbit/high-flight view
/// looks down on the map through clear air instead of a grey wall.
const FOG_FADE_HIGH: f32 = 5000.0;

/// How present the atmospheric ground fog should be at a given camera altitude:
/// 1.0 near the ground, ramping to 0.0 by [`FOG_FADE_HIGH`]. Real haze is densest
/// near the surface; this mirrors that so climbing out reveals the world rather
/// than drowning it in fog (see `apply_distance_fog` / `update_horizon_haze`).
pub fn fog_altitude_factor(altitude_m: f32) -> f32 {
    1.0 - crate::utils::sun::smoothstep(FOG_FADE_LOW, FOG_FADE_HIGH, altitude_m)
}

// Simple constants
const FREE_CAMERA_SPEED: f32 = 45.0;
const FREE_CAMERA_SPEED_FAST: f32 = 180.0;
const MOUSE_SENSITIVITY: f32 = 0.002;
const MIN_CAMERA_HEIGHT: f32 = 0.5;

// ── Earth (orbit) mode tuning ───────────────────────────────────────────────
/// Closest the orbit camera may sit to its ground focus (metres).
const ORBIT_MIN_DISTANCE: f64 = 15.0;
/// Wheel-notch zoom rate (fraction of the target distance per notch);
/// exponential so the step scales with altitude, like Google Earth. Kept gentle —
/// the glide (`ZOOM_GLIDE_RATE`) supplies the sense of speed, so per-notch this
/// only needs to nudge; a big step here made each tick leap in height.
const ORBIT_ZOOM_SPEED: f32 = 0.1;
/// Radians of yaw/tilt per pixel of rotate-drag.
const ORBIT_ROT_SENS: f32 = 0.006;
/// Metres of focus travel per pixel of pan-drag, per metre of camera distance
/// (so a drag moves the ground under the cursor by roughly a constant amount).
const ORBIT_PAN_FACTOR: f64 = 0.0016;
/// Tilt clamp: just off the horizon up to nearly straight down.
const ORBIT_PITCH_MIN: f32 = 0.12;
const ORBIT_PITCH_MAX: f32 = std::f32::consts::FRAC_PI_2 - 0.02;
/// Surface-orbit distance is raised well past the space-entry altitude so zooming
/// out can actually cross into the space tier (ADR-025).
const ORBIT_MAX_DISTANCE_SURFACE: f64 = 250_000.0;

// ── Space tier (ADR-025) ────────────────────────────────────────────────────
/// Climb past this altitude (m) and the orbit pivots the Earth's centre. Kept
/// just above the globe-show altitude so the globe appears essentially as you
/// switch to the centred, look-at-Earth space orbit.
const SPACE_ENTER_ALT: f64 = 55_000.0;
/// Drop back below this altitude and it returns to a ground-focus orbit
/// (hysteresis band with `SPACE_ENTER_ALT`).
const SPACE_EXIT_ALT: f64 = 48_000.0;
/// Farthest the space camera may pull back from the Earth centre (m).
const ORBIT_MAX_DISTANCE_SPACE: f64 = 45_000_000.0;
/// If the globe descent lands farther than this (horizontal metres) from the
/// current anchor, re-anchor the world to the sub-point instead of falling back
/// to the flat home plane at the wrong coordinates (ADR-025, anchor-less model).
const REANCHOR_DISTANCE: f64 = 120_000.0;
/// Exponential rate of the smooth zoom glide (per second): the wheel moves a
/// target distance, and the camera flies toward it at this rate.
const ZOOM_GLIDE_RATE: f64 = 6.0;
/// Default camera near/far (restored outside the space view).
const DEFAULT_NEAR: f32 = 0.1;
const DEFAULT_FAR: f32 = 200_000.0;

/// Absolute world position of a big_space grid cell's origin.
fn cell_origin(cell: &GridCell) -> DVec3 {
    DVec3::new(cell.x as f64, cell.y as f64, cell.z as f64) * WORLD_CELL_EDGE as f64
}

/// Focus→camera offset for spherical orbit params (x = east, z = south, y = up).
fn spherical_offset(yaw: f32, pitch: f32, distance: f64) -> DVec3 {
    let (cp, sp) = (pitch.cos() as f64, pitch.sin() as f64);
    let (sy, cy) = (yaw.sin() as f64, yaw.cos() as f64);
    DVec3::new(sy * cp, sp, cy * cp) * distance
}

/// Set the camera near/far only when they actually change (avoids per-frame
/// change-detection churn on the projection).
fn set_projection(projection: &mut Projection, near: f32, far: f32) {
    if let Projection::Perspective(p) = projection {
        if p.near != near || p.far != far {
            p.near = near;
            p.far = far;
        }
    }
}

/// Camera exposure (EV100) paired with the physical `RAW_SUNLIGHT` sun. Bevy's
/// atmosphere demo uses 13.0; higher = darker overall. This is the main knob for
/// balancing scene brightness against the (now physical) sky.
const ATMOSPHERE_EXPOSURE_EV100: f32 = 13.0;

/// Earth inner radius used by `Atmosphere::earth` (metres). The planet-centre
/// entity sits this far below the local ground so the atmosphere's surface aligns
/// with y = 0. Must match bevy_pbr's `Atmosphere::earth` constant.
const EARTH_ATMOSPHERE_INNER_RADIUS: f32 = 6_360_000.0;

/// Grid cell edge for the world `BigSpace` (ADR-019/021). Large enough that local
/// play rarely recenters; small enough to keep within-cell f32 precise.
pub const WORLD_CELL_EDGE: f32 = 10_000.0;
/// Hysteresis buffer past a cell edge before big_space recenters an entity.
pub const WORLD_SWITCH_THRESHOLD: f32 = 100.0;

/// Spawn the world [`BigSpace`] root with the camera as its [`FloatingOrigin`]
/// (ADR-019). Other content currently spawns at the origin (cell 0) and renders
/// correctly; geo-derived `GridCell` placement arrives with planet-scale streaming.
pub fn setup_camera(
    mut commands: Commands,
    mut camera_state: ResMut<CameraState>,
    mut scattering_media: ResMut<Assets<ScatteringMedium>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    dev: Res<crate::resources::DevPanelState>,
) {
    camera_state.mode = CameraMode::Avatar;
    camera_state.distance = 5.0;
    camera_state.azimuth = 0.0;
    camera_state.pitch = std::f32::consts::PI / 6.0;

    // Earth-preset scattering medium for the built-in atmosphere (Bevy 0.19).
    let medium = scattering_media.add(ScatteringMedium::default());

    commands.spawn_big_space(
        Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD),
        |root| {
            // The root must carry Visibility: renderable children inherit it, and a
            // missing parent Visibility triggers Bevy's B0004 warning per child.
            root.insert(Visibility::default());
            root.spawn_spatial((
                Camera3d::default(),
                // Bevy's default far plane (1 km) clips the streamed tile pyramid
                // and the horizon ground disc; reversed-z depth keeps precision
                // fine at this range.
                Projection::Perspective(PerspectiveProjection {
                    far: 200_000.0,
                    ..default()
                }),
                Transform::from_xyz(0.0, 10.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
                // Bevy 0.19 built-in atmosphere: the camera carries only the render
                // settings; the sky is derived from the sun DirectionalLight direction
                // and the camera's altitude above the planet entity spawned below.
                // AmbientLight is per-camera now; apply_day_night_cycle drives its
                // color/brightness from utils::sun.
                AtmosphereSettings::default(),
                // Physically-based exposure to balance the RAW_SUNLIGHT sun so the
                // scene isn't blown out while the sky is lit correctly.
                Exposure { ev100: ATMOSPHERE_EXPOSURE_EV100 },
                // Sky-driven image-based lighting: surfaces are lit by and reflect
                // the actual atmosphere — the big realism win in Bevy's demo.
                AtmosphereEnvironmentMapLight::default(),
                AmbientLight::default(),
                FreeCamera,
                FloatingOrigin,
                // Atmospheric distance fog: distant buildings fade into haze, with a
                // sun-side glow. Driven by the dev panel slider (apply_distance_fog).
                distance_fog(dev.fog_density, dev.time_of_day),
            ));

            // A ground disc out to the horizon, in the OSM basemap's paper tone.
            // Without it, the world ends at the loaded tiles and the void below
            // the horizon reads as a black gulf; with it, DistanceFog fades the
            // ground into the haze so fog and sky meet seamlessly at any altitude.
            root.spawn_spatial((
                Mesh3d(meshes.add(Circle::new(150_000.0))),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::srgb(0.83, 0.83, 0.80),
                    perceptual_roughness: 1.0,
                    reflectance: 0.0,
                    ..default()
                })),
                // Well below the whole tile pyramid (whose coarsest rings sit a few
                // metres down — see update_map_stream) so it never covers a tile.
                Transform::from_xyz(0.0, -5.0, 0.0)
                    .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
                NotShadowCaster,
                // From orbit this 150 km disc reads as a bright circle over the
                // anchor; hide it (and the tiles/regions) while the globe is up.
                crate::systems::globe::HideInSpace,
            ));
        },
    );

    // The built-in atmosphere is planetary-scale; its origin and radius come from
    // this entity's transform. Placed one Earth radius straight down so the surface
    // (`Atmosphere::earth` inner_radius) sits at the local ground plane (y = 0), so
    // the camera reads at its true altitude above it. A plain entity (not under the
    // BigSpace): the camera is the floating origin and stays near y = 0, so the sky's
    // altitude stays effectively constant — correct for the flat local world.
    commands.spawn((
        Atmosphere::earth(medium),
        Transform::from_xyz(0.0, -EARTH_ATMOSPHERE_INNER_RADIUS, 0.0),
    ));
}

pub fn camera_mode_toggle(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut camera_state: ResMut<CameraState>,
    camera_query: Query<&Transform, With<FreeCamera>>,
) {
    if keyboard_input.just_pressed(KeyCode::Tab) {
        // Cycle Avatar → Free → Earth (orbit) → Avatar.
        camera_state.mode = match camera_state.mode {
            CameraMode::Avatar => {
                // Initialize free camera rotation from current camera orientation
                if let Ok(camera_transform) = camera_query.single() {
                    let euler = camera_transform.rotation.to_euler(EulerRot::YXZ);
                    camera_state.free_camera_rotation = Vec2::new(euler.1, euler.0);
                }
                CameraMode::Free
            }
            CameraMode::Free => {
                // Re-seed the orbit focus from the current view next frame.
                camera_state.orbit_initialized = false;
                CameraMode::Orbit
            }
            CameraMode::Orbit => CameraMode::Avatar,
        };
        println!("Camera mode: {:?}", camera_state.mode);
    }
}

pub fn camera_controls(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mouse_input: Res<ButtonInput<MouseButton>>,
    mut cursor_moved_events: MessageReader<CursorMoved>,
    mut mouse_wheel_events: MessageReader<bevy::input::mouse::MouseWheel>,
    mut camera_query: Query<(&mut GridCell, &mut Transform, &mut Projection), With<FreeCamera>>,
    mut camera_state: ResMut<CameraState>,
    avatar_state: Res<AvatarState>,
    time: Res<Time>,
    region_mesh_query: Query<&GlobalTransform, With<RegionMesh>>,
    gizmo_state: Res<GizmoState>,
    egui_manager: Res<EguiManager>,
    globe: Res<crate::systems::globe::GlobeState>,
    windows: Query<&Window>,
    // Read-only view of the same camera for viewport→ray conversion (disjoint
    // components from the mutable query above).
    cam_render: Query<(&Camera, &GlobalTransform), With<FreeCamera>>,
) {
    if camera_query.is_empty() {
        return;
    }

    let Ok((mut camera_cell, mut camera_transform, mut projection)) = camera_query.single_mut()
    else {
        return;
    };
    let delta_time = time.delta().as_secs_f32();

    // Keep the near/far at their ground defaults unless the space tier overrides
    // them below (Avatar/Free never touch the projection otherwise).
    set_projection(&mut projection, DEFAULT_NEAR, DEFAULT_FAR);

    match camera_state.mode {
        CameraMode::Avatar => {
            // Third-person camera following avatar (smoothed when online — see `AvatarState::display_position`)
            let avatar_pos = avatar_state.display_position;

            // Handle mouse wheel zoom
            for event in mouse_wheel_events.read() {
                camera_state.distance += event.y * 0.1;
                camera_state.distance = camera_state.distance.max(2.0).min(100.0);
            }

            // Handle mouse drag for rotation (suppressed while egui, gizmo, or Ctrl has the pointer).
            let ctrl = keyboard_input.pressed(KeyCode::ControlLeft)
                || keyboard_input.pressed(KeyCode::ControlRight);
            if gizmo_state.active_axis.is_some()
                || egui_manager.ctx.egui_wants_pointer_input()
                || ctrl
            {
                camera_state.pan_offset = None;
                cursor_moved_events.clear();
            } else if mouse_input.pressed(MouseButton::Left) {
                for event in cursor_moved_events.read() {
                    if let Some(last_pos) = camera_state.pan_offset {
                        let delta = event.position - last_pos;
                        camera_state.azimuth -= delta.x * 0.005;
                        camera_state.pitch -= delta.y * 0.005;
                        camera_state.pitch = camera_state.pitch
                            .max(-std::f32::consts::PI / 3.0)
                            .min(std::f32::consts::PI / 2.5);
                    }
                    camera_state.pan_offset = Some(event.position);
                }
            } else if mouse_input.just_released(MouseButton::Left) {
                camera_state.pan_offset = None;
            }

            // Calculate camera position
            let horizontal_distance = camera_state.distance * camera_state.pitch.cos();
            let vertical_offset = camera_state.distance * camera_state.pitch.sin();

            let camera_offset = Vec3::new(
                camera_state.azimuth.sin() * horizontal_distance,
                vertical_offset + 1.5,
                camera_state.azimuth.cos() * horizontal_distance,
            );

            let mut target_position = avatar_pos + camera_offset;

            // Prevent camera from going below ground
            let ground_height = get_ground_height(target_position, &region_mesh_query);
            let min_height = ground_height + MIN_CAMERA_HEIGHT;
            if target_position.y < min_height {
                target_position.y = min_height;
            }

            // Framerate-independent smoothing so orbit matches the follow target
            let cam_alpha = 1.0 - (-12.0_f32 * delta_time).exp();
            camera_transform.translation = camera_transform.translation.lerp(target_position, cam_alpha);
            camera_transform.look_at(avatar_pos + Vec3::Y * 1.5, Vec3::Y);
        }
        CameraMode::Free => {
            // Free camera mode - simple FPS style
            let mut rotation_delta = Vec2::ZERO;

            // Mouse look (left mouse button — right is reserved for context menu).
            // Suppressed while egui, gizmo, or Ctrl has the pointer.
            let ctrl = keyboard_input.pressed(KeyCode::ControlLeft)
                || keyboard_input.pressed(KeyCode::ControlRight);
            if gizmo_state.active_axis.is_some()
                || egui_manager.ctx.egui_wants_pointer_input()
                || ctrl
            {
                camera_state.pan_offset = None;
                cursor_moved_events.clear();
            } else if mouse_input.pressed(MouseButton::Left) {
                for event in cursor_moved_events.read() {
                    if let Some(last_pos) = camera_state.pan_offset {
                        let delta = event.position - last_pos;
                        rotation_delta.x += delta.x;
                        rotation_delta.y += delta.y;
                    }
                    camera_state.pan_offset = Some(event.position);
                }
            } else {
                camera_state.pan_offset = None;
            }

            // Update rotation from mouse input
            if rotation_delta.length() > 0.0 {
                camera_state.free_camera_rotation.y -= rotation_delta.x * MOUSE_SENSITIVITY;
                camera_state.free_camera_rotation.x -= rotation_delta.y * MOUSE_SENSITIVITY;
                camera_state.free_camera_rotation.x = camera_state.free_camera_rotation.x
                    .max(-std::f32::consts::PI / 2.0 + 0.1)
                    .min(std::f32::consts::PI / 2.0 - 0.1);
            }

            // Apply rotation
            let rotation_quat = Quat::from_euler(
                EulerRot::YXZ,
                camera_state.free_camera_rotation.y,
                camera_state.free_camera_rotation.x,
                0.0,
            );
            camera_transform.rotation = rotation_quat;

            // Movement - calculate forward/right from rotation, keep vertical independent
            let forward = rotation_quat * -Vec3::Z;
            let right = rotation_quat * Vec3::X;

            // Flatten to horizontal plane for WASD movement
            let forward_horizontal = Vec3::new(forward.x, 0.0, forward.z).normalize();
            let right_horizontal = Vec3::new(right.x, 0.0, right.z).normalize();

            let speed = if keyboard_input.pressed(KeyCode::ShiftLeft) || keyboard_input.pressed(KeyCode::ShiftRight) {
                FREE_CAMERA_SPEED_FAST
            } else {
                FREE_CAMERA_SPEED
            };

            let mut move_direction = Vec3::ZERO;

            if keyboard_input.pressed(KeyCode::KeyW) {
                move_direction += forward_horizontal;
            }
            if keyboard_input.pressed(KeyCode::KeyS) {
                move_direction -= forward_horizontal;
            }
            if keyboard_input.pressed(KeyCode::KeyA) {
                move_direction -= right_horizontal;
            }
            if keyboard_input.pressed(KeyCode::KeyD) {
                move_direction += right_horizontal;
            }

            // Vertical movement - ALWAYS world-space, never affects rotation
            if keyboard_input.pressed(KeyCode::Space) {
                move_direction += Vec3::Y;
            }
            // Q or Shift for moving down (but Shift also speeds up, so only use Shift if not moving horizontally)
            if keyboard_input.pressed(KeyCode::KeyQ) {
                move_direction -= Vec3::Y;
            }
            // Shift alone (without WASD) moves down
            if (keyboard_input.pressed(KeyCode::ShiftLeft) || keyboard_input.pressed(KeyCode::ShiftRight))
                && !keyboard_input.pressed(KeyCode::KeyW)
                && !keyboard_input.pressed(KeyCode::KeyA)
                && !keyboard_input.pressed(KeyCode::KeyS)
                && !keyboard_input.pressed(KeyCode::KeyD) {
                move_direction -= Vec3::Y;
            }

            // Suppress keyboard-driven movement while the UI owns the keyboard (text
            // field focused or a Ctrl chord like Ctrl+Shift+A).
            if egui_manager.ui_owns_keyboard(false, &keyboard_input) {
                move_direction = Vec3::ZERO;
            }

            // Apply movement
            if move_direction.length() > 0.0 {
                move_direction = move_direction.normalize();
                camera_transform.translation += move_direction * speed * delta_time;
            }

            // Prevent going below ground
            let ground_height = get_ground_height(camera_transform.translation, &region_mesh_query);
            let min_height = ground_height + MIN_CAMERA_HEIGHT;
            if camera_transform.translation.y < min_height {
                camera_transform.translation.y = min_height;
            }
        }
        CameraMode::Orbit => {
            // "Earth" mode: a Google-Earth-style orbit. Near the ground it pivots
            // a fixed ground point (left-drag pans, middle/Ctrl+left rotates+tilts,
            // wheel zooms). Zoom out far enough and it hands off to the space tier
            // (ADR-025): the pivot becomes the Earth's centre and left-drag spins
            // the whole globe. The altitude drives map LOD, so pulling back
            // coarsens the imagery and, past the show altitude, reveals the planet.

            // On entry, seed the ground focus/distance/angles from wherever the
            // camera was looking, so switching modes doesn't jump the view.
            if !camera_state.orbit_initialized {
                let cam_world = cell_origin(&camera_cell) + camera_transform.translation.as_dvec3();
                let forward = (camera_transform.rotation * Vec3::NEG_Z).as_dvec3();
                // Intersect the view ray with the ground plane (y = 0); if the
                // camera looks level or up, drop the focus straight below it.
                let focus = if forward.y < -1e-3 {
                    let t = cam_world.y / -forward.y;
                    let hit = cam_world + forward * t;
                    DVec3::new(hit.x, 0.0, hit.z)
                } else {
                    DVec3::new(cam_world.x, 0.0, cam_world.z)
                };
                let offset = cam_world - focus;
                let dist = offset.length().clamp(ORBIT_MIN_DISTANCE, ORBIT_MAX_DISTANCE_SURFACE);
                camera_state.orbit_focus = focus;
                camera_state.orbit_distance = dist;
                camera_state.orbit_distance_target = dist;
                camera_state.orbit_yaw = (offset.x).atan2(offset.z) as f32;
                camera_state.orbit_pitch = ((offset.y / dist).clamp(-1.0, 1.0) as f32)
                    .asin()
                    .clamp(ORBIT_PITCH_MIN, ORBIT_PITCH_MAX);
                camera_state.orbit_space = false;
                camera_state.orbit_initialized = true;
            }

            let center = globe.center_local.as_dvec3();
            let radius = globe.radius as f64;

            // Surface↔space handoff. Reparameterise about the new pivot from the
            // unchanged camera world position, so the camera does not move; the
            // zoom target carries over as a ratio so an in-flight glide continues
            // smoothly through the transition.
            if globe.ready {
                let cam_world = if camera_state.orbit_space {
                    center + camera_state.orbit_dir * camera_state.orbit_distance
                } else {
                    camera_state.orbit_focus
                        + spherical_offset(
                            camera_state.orbit_yaw,
                            camera_state.orbit_pitch,
                            camera_state.orbit_distance,
                        )
                };
                let alt = (cam_world - center).length() - radius;
                if !camera_state.orbit_space && alt > SPACE_ENTER_ALT {
                    let off = cam_world - center;
                    let d = off.length();
                    let ratio = camera_state.orbit_distance_target / camera_state.orbit_distance;
                    camera_state.orbit_space = true;
                    camera_state.orbit_distance = d;
                    camera_state.orbit_distance_target = d * ratio;
                    // Free-arcball orientation: direction from the Earth's centre
                    // plus the camera's current up, so the view doesn't jump.
                    camera_state.orbit_dir = off / d;
                    camera_state.orbit_up = (camera_transform.rotation * Vec3::Y).as_dvec3();
                } else if camera_state.orbit_space && alt < SPACE_EXIT_ALT {
                    // How far the descent point is from the home anchor (origin).
                    let horiz = (cam_world.x * cam_world.x + cam_world.z * cam_world.z).sqrt();
                    if horiz > REANCHOR_DISTANCE && camera_state.reanchor_request.is_none() {
                        // Descending onto a different part of the globe: re-anchor
                        // the world to the sub-point (handled by a teleport next
                        // frame) rather than snapping to the home plane far away.
                        let dir = (cam_world - center).normalize_or_zero().as_vec3();
                        let (slat, slng) = crate::systems::globe::sub_point(
                            globe.anchor_lat,
                            globe.anchor_lng,
                            dir,
                        );
                        camera_state.reanchor_request =
                            Some((slat, slng, alt.max(SPACE_EXIT_ALT)));
                        // Stay in space this frame; the teleport repositions us.
                    } else if horiz <= REANCHOR_DISTANCE {
                        // Near home: hand back to the ground-focus orbit as before.
                        let focus = DVec3::new(cam_world.x, 0.0, cam_world.z);
                        let off = cam_world - focus;
                        let d = off.length().max(ORBIT_MIN_DISTANCE);
                        let ratio =
                            camera_state.orbit_distance_target / camera_state.orbit_distance;
                        camera_state.orbit_space = false;
                        camera_state.orbit_focus = focus;
                        camera_state.orbit_distance = d;
                        camera_state.orbit_distance_target = (d * ratio).max(ORBIT_MIN_DISTANCE);
                        camera_state.orbit_yaw = off.x.atan2(off.z) as f32;
                        camera_state.orbit_pitch = ((off.y / d).clamp(-1.0, 1.0) as f32)
                            .asin()
                            .clamp(ORBIT_PITCH_MIN, ORBIT_PITCH_MAX);
                    }
                }
            }
            let space = camera_state.orbit_space;

            // Gather drag deltas, split by gesture. In space, left-drag spins the
            // globe (rotate); on the surface it pans the map, and rotate is on the
            // middle button or Ctrl+left. Suppressed while egui/gizmo owns pointer.
            let mut rot_delta = Vec2::ZERO;
            let mut pan_delta = Vec2::ZERO;
            if gizmo_state.active_axis.is_some() || egui_manager.ctx.egui_wants_pointer_input() {
                camera_state.pan_offset = None;
                cursor_moved_events.clear();
            } else {
                let ctrl = keyboard_input.pressed(KeyCode::ControlLeft)
                    || keyboard_input.pressed(KeyCode::ControlRight);
                let rotating = mouse_input.pressed(MouseButton::Middle)
                    || (ctrl && mouse_input.pressed(MouseButton::Left))
                    || (space && mouse_input.pressed(MouseButton::Left));
                let panning = !rotating && !space && mouse_input.pressed(MouseButton::Left);
                if rotating || panning {
                    for event in cursor_moved_events.read() {
                        if let Some(last) = camera_state.pan_offset {
                            let delta = event.position - last;
                            if rotating {
                                rot_delta += delta;
                            } else {
                                pan_delta += delta;
                            }
                        }
                        camera_state.pan_offset = Some(event.position);
                    }
                } else {
                    camera_state.pan_offset = None;
                    cursor_moved_events.clear();
                }
            }

            // Wheel zoom: exponential toward/away from the pivot; clamps differ by
            // tier (ground metres vs orbit distance from the Earth centre).
            let (min_d, max_d) = if space {
                // Down to the surface (alt 0); the exit handoff fires at
                // SPACE_EXIT_ALT well before the camera reaches it.
                (radius, ORBIT_MAX_DISTANCE_SPACE)
            } else {
                (ORBIT_MIN_DISTANCE, ORBIT_MAX_DISTANCE_SURFACE)
            };
            // The wheel moves a *target*; the camera glides toward it below, so
            // zoom reads as a smooth fly instead of a per-notch height jump.
            // In space the factor applies to the *altitude*, not the distance
            // from the Earth's centre — otherwise one notch near the surface is
            // 10% of ~6,400 km (a violent 640 km plunge) instead of 10% of the
            // height above the ground.
            for event in mouse_wheel_events.read() {
                let factor = (1.0 - event.y * ORBIT_ZOOM_SPEED).clamp(0.2, 5.0) as f64;
                camera_state.orbit_distance_target = if space {
                    let alt = (camera_state.orbit_distance_target - radius).max(500.0);
                    (radius + alt * factor).clamp(min_d, max_d)
                } else {
                    (camera_state.orbit_distance_target * factor).clamp(min_d, max_d)
                };
            }
            let glide = 1.0 - (-ZOOM_GLIDE_RATE * delta_time as f64).exp();
            let old_d = camera_state.orbit_distance;
            camera_state.orbit_distance =
                (old_d + (camera_state.orbit_distance_target - old_d) * glide).clamp(min_d, max_d);
            // Per-frame zoom fraction (< 1 while flying in), which also paces the
            // cursor homing so it tracks the glide rather than the wheel notches.
            let zoom_factor = camera_state.orbit_distance / old_d;

            // Zoom-toward-cursor (space): scrolling in homes the orbit onto the
            // point under the cursor — cast the cursor ray onto the sphere and
            // pull the pivot direction toward the hit by the zoom fraction. This
            // is how you dive back to a specific spot on the globe (centre-locked
            // zoom made re-finding a place nearly impossible).
            if space && zoom_factor < 0.999 && !egui_manager.ctx.egui_wants_pointer_input() {
                let cursor_hit = windows
                    .iter()
                    .next()
                    .and_then(|w| w.cursor_position())
                    .zip(cam_render.single().ok())
                    .and_then(|(cursor, (cam, cam_gt))| {
                        // Proven ray construction (same call picking uses). The ray
                        // is in render space — world shifted by the camera's grid
                        // cell — so shift the sphere centre instead of the ray.
                        let ray = cam.viewport_to_world(cam_gt, cursor).ok()?;
                        let center_render = center - cell_origin(&camera_cell);
                        let origin = ray.origin.as_dvec3();
                        let dir = ray.direction.as_vec3().as_dvec3();
                        let oc = origin - center_render;
                        let b = oc.dot(dir);
                        let disc = b * b - (oc.length_squared() - radius * radius);
                        if disc < 0.0 {
                            return None; // cursor misses the globe
                        }
                        let t = -b - disc.sqrt();
                        // Direction from the centre is translation-invariant, so
                        // it's valid in the world frame too.
                        (t > 0.0).then(|| (origin + dir * t - center_render).normalize())
                    });
                if let Some(hit_dir) = cursor_hit {
                    // Pull the orbit toward the cursor point at the pace of the
                    // glide; the up vector rides the same rotation, so there is
                    // no pole/clamp anywhere on the way.
                    let blend = ((1.0 - zoom_factor) * 1.5).clamp(0.0, 0.5);
                    let new_dir = camera_state.orbit_dir.lerp(hit_dir, blend).normalize();
                    let rot = DQuat::from_rotation_arc(camera_state.orbit_dir, new_dir);
                    camera_state.orbit_dir = new_dir;
                    camera_state.orbit_up = (rot * camera_state.orbit_up).normalize();
                }
            }

            // Apply rotation. Space: a free arcball — horizontal drag rotates the
            // orbit about the camera's up, vertical about its right; up rides
            // along, so there is no pole and no lock in any direction. Surface:
            // the yaw/pitch turntable as before.
            if rot_delta != Vec2::ZERO {
                if space {
                    // Grab-the-globe: scale radians-per-pixel so the ground under
                    // the cursor tracks the drag at any distance. Two regimes,
                    // take whichever is smaller:
                    //  - near: the visible ground patch subtends ~fov·(alt/R) of
                    //    central angle, so rotation shrinks with altitude (the old
                    //    far-field-only formula stayed too fast in the mid-range);
                    //  - far: the whole globe spans 2·asin(R/d) of the view, so a
                    //    full-window drag sweeps about one globe-height.
                    let h = windows.iter().next().map(|w| w.height()).unwrap_or(800.0);
                    let fov = match &*projection {
                        Projection::Perspective(p) => p.fov,
                        _ => std::f32::consts::FRAC_PI_4,
                    };
                    let d = camera_state.orbit_distance;
                    let near_arc = fov * (((d - radius) / radius).max(0.0) as f32);
                    let far_arc =
                        2.4 * ((radius / d).clamp(0.0, 1.0) as f32).asin();
                    let sens = (1.2 * near_arc.min(far_arc) / h).clamp(2e-6, ORBIT_ROT_SENS);
                    let up = camera_state.orbit_up;
                    let right = (-camera_state.orbit_dir).cross(up).normalize_or_zero();
                    let rot = DQuat::from_axis_angle(up, -(rot_delta.x * sens) as f64)
                        * DQuat::from_axis_angle(right, -(rot_delta.y * sens) as f64);
                    camera_state.orbit_dir = (rot * camera_state.orbit_dir).normalize();
                    camera_state.orbit_up = (rot * camera_state.orbit_up).normalize();
                } else {
                    camera_state.orbit_yaw -= rot_delta.x * ORBIT_ROT_SENS;
                    camera_state.orbit_pitch = (camera_state.orbit_pitch
                        + rot_delta.y * ORBIT_ROT_SENS)
                        .clamp(ORBIT_PITCH_MIN, ORBIT_PITCH_MAX);
                }
            }

            // Google-Earth-style auto-tilt (surface orbit): pulling back raises a
            // pitch floor toward top-down on a log ramp (2 km → 60 km), so zoom-out
            // always lifts you *over* the map instead of leaving the camera parked
            // at a grazing angle staring at the horizon/atmosphere. Tilting freely
            // is still allowed up close, where oblique views are useful.
            if !space {
                let t = ((camera_state.orbit_distance / 2_000.0).ln()
                    / (60_000.0f64 / 2_000.0).ln())
                .clamp(0.0, 1.0) as f32;
                let floor = ORBIT_PITCH_MIN + (1.30 - ORBIT_PITCH_MIN) * t;
                if camera_state.orbit_pitch < floor {
                    camera_state.orbit_pitch = floor;
                }
            }

            let (yaw, pitch) = (camera_state.orbit_yaw, camera_state.orbit_pitch);
            let offset = spherical_offset(yaw, pitch, camera_state.orbit_distance);

            // Pan (surface only): grab-and-drag the ground — the map follows the
            // cursor on BOTH axes. The basis comes straight from the orbit yaw
            // (exact even at near-nadir tilt, where a flattened camera-forward
            // degenerates): `fwd_flat` is the horizontal camera→focus direction,
            // i.e. "screen-up on the ground".
            if pan_delta != Vec2::ZERO {
                let (sy, cy) = (camera_state.orbit_yaw.sin(), camera_state.orbit_yaw.cos());
                let fwd_flat = Vec3::new(-sy, 0.0, -cy);
                let right = Vec3::new(cy, 0.0, -sy);
                let scale = (camera_state.orbit_distance * ORBIT_PAN_FACTOR) as f32;
                // Drag right → rig moves left → content follows the cursor right;
                // drag down → rig moves screen-up on the ground → content slides
                // down. (The vertical axis used to move the rig the other way —
                // grab horizontally but inverted vertically, which is why panning
                // felt "vice versa" up close while the space globe felt right.)
                let shift =
                    right * (-pan_delta.x * scale) + fwd_flat * (pan_delta.y * scale);
                camera_state.orbit_focus += shift.as_dvec3();
                camera_state.orbit_focus.y = 0.0;
            }

            // Place the camera: pivot + offset → big_space cell + local, then look
            // at the pivot (ground focus, or the Earth centre in space). The space
            // view uses the arcball's own up (re-orthonormalized against drift) —
            // a fixed world-up is what created the old pole locks.
            let (pivot, cam_world, look_up) = if space {
                let dir = camera_state.orbit_dir;
                let mut up = camera_state.orbit_up - dir * camera_state.orbit_up.dot(dir);
                if up.length_squared() < 1e-12 {
                    up = DVec3::Y; // degenerate after extreme drags: re-seed
                }
                camera_state.orbit_up = up.normalize();
                (
                    center,
                    center + dir * camera_state.orbit_distance,
                    camera_state.orbit_up.as_vec3(),
                )
            } else {
                (
                    camera_state.orbit_focus,
                    camera_state.orbit_focus + offset,
                    Vec3::Y,
                )
            };
            let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
            let (new_cell, local) = grid.translation_to_grid(cam_world);
            *camera_cell = new_cell;
            camera_transform.translation = local;
            let pivot_local = (pivot - cell_origin(&new_cell)).as_vec3();
            camera_transform.look_at(pivot_local, look_up);

            // Depth range: once the globe is in view (high surface orbit or space),
            // push the far plane past the far limb and raise the near plane with
            // altitude so nothing relevant clips. Otherwise the ground defaults.
            if globe.ready {
                let altitude = (cam_world - center).length() - radius;
                if altitude as f32 > crate::systems::globe::GLOBE_SHOW_ALT {
                    let dist_center = (cam_world - center).length() as f32;
                    // Keep `near` below the star/moon shells (~150 km) so they stay
                    // visible from deep space; reversed-z keeps depth precise even
                    // with a small near against the far-limb `far`.
                    let near = (altitude as f32 * 0.01).clamp(100.0, 50_000.0);
                    let far = (dist_center + globe.radius * 2.5).clamp(DEFAULT_FAR, 60_000_000.0);
                    set_projection(&mut projection, near, far);
                }
            }
        }
    }
}

fn get_ground_height(position: Vec3, region_mesh_query: &Query<&GlobalTransform, With<RegionMesh>>) -> f32 {
    let mut min_height: f32 = 0.0;
    for region_transform in region_mesh_query.iter() {
        let region_y: f32 = region_transform.translation().y;
        let region_top: f32 = region_y + 0.05;
        if position.x >= region_transform.translation().x - 150.0
            && position.x <= region_transform.translation().x + 150.0
            && position.z >= region_transform.translation().z - 150.0
            && position.z <= region_transform.translation().z + 150.0 {
            min_height = min_height.max(region_top);
        }
    }
    min_height
}
