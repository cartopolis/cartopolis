use bevy::pbr::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy_atmosphere::plugin::AtmosphereSkyBox;
use bevy_atmosphere::prelude::*;
use bevy_atmosphere::skybox::{self, AtmosphereSkyBoxMaterial};
use big_space::prelude::{BigSpaceCommands, FloatingOrigin, Grid};

use crate::resources::{AvatarState, CameraState, CameraMode};
use crate::systems::egui_manager::EguiManager;
use crate::systems::gizmo::GizmoState;
use crate::systems::rendering::RegionMesh;

#[derive(Component)]
pub struct FreeCamera;

/// Builds the camera's [`DistanceFog`] for a dev-panel density in 0.0–1.0 and the
/// sun's elevation (`sun_direction().y`). The falloff is exponential-squared so the
/// near field stays clear and geometry vanishes into a fog "wall"; the fog color
/// tracks day/night so it reads as bright haze by day and near-black murk at night.
pub fn distance_fog(density: f32, sun_elevation: f32) -> DistanceFog {
    let day = (sun_elevation / 0.2).clamp(0.0, 1.0);
    let night_color = Vec3::new(0.015, 0.02, 0.035);
    // Slightly desaturated toward white so the fog wall meets the Mie-hazed sky.
    let day_color = Vec3::new(0.71, 0.75, 0.83);
    let c = night_color.lerp(day_color, day);

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
        // Warm sun-side glow in the fog; fades out with the sun.
        directional_light_color: Color::srgba(1.0, 0.95, 0.85, 0.5 * day),
        directional_light_exponent: 30.0,
        falloff,
    }
}

// Simple constants
const FREE_CAMERA_SPEED: f32 = 45.0;
const FREE_CAMERA_SPEED_FAST: f32 = 180.0;
const MOUSE_SENSITIVITY: f32 = 0.002;
const MIN_CAMERA_HEIGHT: f32 = 0.5;

/// Size parameter for the manually-spawned atmosphere skybox mesh (the `detection`
/// feature is off — see CLAUDE.md). The cube's faces sit at ~0.707× this value and
/// the mesh **writes depth**, so it occludes everything farther away; it must be
/// sized beyond the whole world (tile pyramid ~20 km, ground disc 150 km). The sky
/// shader samples by view direction, so the size is visually irrelevant.
const SKYBOX_MESH_FAR: f32 = 300_000.0;

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
    sky_material: Res<AtmosphereSkyBoxMaterial>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    dev: Res<crate::resources::DevPanelState>,
) {
    camera_state.mode = CameraMode::Avatar;
    camera_state.distance = 5.0;
    camera_state.azimuth = 0.0;
    camera_state.pitch = std::f32::consts::PI / 6.0;

    let skybox_mesh = meshes.add(skybox::mesh(SKYBOX_MESH_FAR));
    let sky_handle = sky_material.0.clone();

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
                AtmosphereCamera::default(),
                FreeCamera,
                FloatingOrigin,
                // Atmospheric distance fog: distant buildings fade into haze, with a
                // sun-side glow. Driven by the dev panel slider (apply_distance_fog).
                distance_fog(
                    dev.fog_density,
                    crate::utils::sun::sun_direction(dev.time_of_day).y,
                ),
            ))
            .with_child((
                Mesh3d(skybox_mesh),
                MeshMaterial3d(sky_handle),
                AtmosphereSkyBox,
                NotShadowCaster,
                NotShadowReceiver,
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
            ));
        },
    );
}

pub fn camera_mode_toggle(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut camera_state: ResMut<CameraState>,
    camera_query: Query<&Transform, With<FreeCamera>>,
) {
    if keyboard_input.just_pressed(KeyCode::Tab) {
        camera_state.mode = match camera_state.mode {
            CameraMode::Avatar => {
                // Initialize free camera rotation from current camera orientation
                if let Ok(camera_transform) = camera_query.single() {
                    let euler = camera_transform.rotation.to_euler(EulerRot::YXZ);
                    camera_state.free_camera_rotation = Vec2::new(euler.1, euler.0);
                }
                CameraMode::Free
            }
            CameraMode::Free => CameraMode::Avatar,
        };
        println!("Camera mode: {:?}", camera_state.mode);
    }
}

pub fn camera_controls(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mouse_input: Res<ButtonInput<MouseButton>>,
    mut cursor_moved_events: EventReader<CursorMoved>,
    mut mouse_wheel_events: EventReader<bevy::input::mouse::MouseWheel>,
    mut camera_query: Query<&mut Transform, With<FreeCamera>>,
    mut camera_state: ResMut<CameraState>,
    avatar_state: Res<AvatarState>,
    time: Res<Time>,
    region_mesh_query: Query<&GlobalTransform, With<RegionMesh>>,
    gizmo_state: Res<GizmoState>,
    egui_manager: Res<EguiManager>,
) {
    if camera_query.is_empty() {
        return;
    }

    let Ok(mut camera_transform) = camera_query.single_mut() else {
        return;
    };
    let delta_time = time.delta().as_secs_f32();

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
                || egui_manager.ctx.wants_pointer_input()
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
                || egui_manager.ctx.wants_pointer_input()
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
