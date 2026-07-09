use bevy::asset::AssetPlugin;
use bevy::log::LogPlugin;
use bevy::pbr::light_consts::lux::AMBIENT_DAYLIGHT;
use bevy::pbr::{
    CascadeShadowConfigBuilder, DirectionalLightShadowMap, Material, MaterialPipeline,
    MaterialPipelineKey, MaterialPlugin,
};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderRef, SpecializedMeshPipelineError,
};
use bevy_atmosphere::prelude::*;
use clap::Parser;
use std::path::PathBuf;

mod components;
mod db;
mod resources;
mod systems;
mod utils;

use components::Avatar;
use resources::{
    AiAssistantState, AiConfig, AvatarState, CameraState, ConnectAddr, ContextMenuState, Database,
    DevPanelState, EditDialogState, GameState, LocalAvatarSimId, MarqueeState, MouseState,
    OsmTileUrlTemplate, PrimTextureCache, TextureLibrary,
};
use big_space::prelude::{BigSpace, GridCell};
use systems::egui_manager::EguiPlugin;
use systems::*;
use utils::sun::sun_direction;

#[derive(Parser, Debug)]
#[command(name = "viberfox")]
struct Cli {
    /// Connect to a `vibe_sim` instance (TCP, postcard messages).
    #[arg(long)]
    connect: Option<String>,
}

fn main() {
    dotenvy::dotenv().ok();

    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off,viberfox=info")),
        )
        .init();

    let mut app = App::new();
    let asset_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .display()
        .to_string();
    let default_plugins = DefaultPlugins
        .build()
        .disable::<LogPlugin>()
        .set(WindowPlugin {
            primary_window: Some(Window {
                title: "Viberfox".into(),
                resolution: (1280.0, 720.0).into(),
                ..default()
            }),
            ..default()
        })
        .set(AssetPlugin {
            file_path: asset_dir,
            ..default()
        })
        // big_space owns transform propagation (ADR-019); Bevy's TransformPlugin
        // must be disabled or big_space errors at startup.
        .disable::<bevy::transform::TransformPlugin>();

    app.add_plugins(default_plugins)
    .add_plugins(big_space::prelude::BigSpaceDefaultPlugins)
    .add_plugins(EguiPlugin)
    .add_plugins(AtmospherePlugin)
    .add_plugins(MaterialPlugin::<StarSkyMaterial>::default())
    .add_plugins(MaterialPlugin::<HorizonHazeMaterial>::default())
    .insert_resource(AtmosphereModel::default())
    .init_resource::<GameState>()
    .init_resource::<AvatarState>()
    .init_resource::<LocalAvatarSimId>()
    .init_resource::<CameraState>()
    .init_resource::<MouseState>()
    .init_resource::<systems::tile_loader::TileCache>()
    .init_resource::<OsmTileUrlTemplate>()
    .init_resource::<ContextMenuState>()
    .init_resource::<EditDialogState>()
    .init_resource::<systems::gizmo::GizmoState>()
    .init_resource::<MarqueeState>()
    .init_resource::<TextureLibrary>()
    .init_resource::<PrimTextureCache>()
    .insert_resource(AiConfig {
        api_key: std::env::var("ANTHROPIC_KEY").ok(),
        model: std::env::var("ANTHROPIC_MODEL")
            .unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string()),
    })
    .init_resource::<AiAssistantState>()
    .init_resource::<DevPanelState>()
    .init_resource::<systems::osm_buildings::OsmBuildings>()
    .init_resource::<systems::map_stream::MapStream>()
    .insert_resource(DirectionalLightShadowMap { size: 4096 });

    if let Some(addr) = cli.connect {
        app.insert_resource(ConnectAddr(addr));
    }

    app.add_systems(
        Startup,
        (
            database::init_database.run_if(no_connect_addr),
            database::load_local_textures.run_if(no_connect_addr),
            network::spawn_network_thread.run_if(has_connect_addr),
            systems::free_camera::setup_camera,
            spawn_avatar_entity,
            setup_sky,
            setup_stars,
            setup_horizon_haze,
        ),
    )
    .add_systems(
        Update,
        (
            network::apply_network_snapshot,
            network::create_egui_texture_handles
                .after(network::apply_network_snapshot),
            database::load_regions
                .run_if(has_database)
                .after(network::apply_network_snapshot),
            database::load_prims
                .run_if(has_database)
                .after(network::apply_network_snapshot),
            rendering::spawn_regions
                .after(database::load_regions)
                .after(network::apply_network_snapshot),
            rendering::spawn_prims
                .after(database::load_prims)
                .after(network::apply_network_snapshot),
            rendering::rebuild_prim_meshes
                .after(rendering::spawn_prims),
            rendering::refresh_prim_textures
                .after(rendering::spawn_prims)
                .after(network::apply_network_snapshot),
        ),
    )
    .add_systems(
        Update,
        (
            systems::tile_loader::load_region_tiles,
            rendering::update_region_materials,
            systems::osm_buildings::update_buildings.after(rendering::spawn_regions),
            systems::map_stream::init_map_stream.after(rendering::spawn_regions),
            systems::map_stream::update_map_stream.after(systems::map_stream::init_map_stream),
            systems::map_stream::apply_stream_textures.after(systems::map_stream::update_map_stream),
            attach_avatar_to_bigspace,
        ),
    )
    .add_systems(
        Update,
        (
            systems::free_camera::camera_mode_toggle,
            avatar::handle_avatar_movement.after(network::apply_network_snapshot),
            avatar::smooth_online_avatar_display
                .after(network::apply_network_snapshot)
                .after(avatar::handle_avatar_movement),
            avatar::smooth_remote_avatars.after(avatar::smooth_online_avatar_display),
            avatar::tick_remote_avatar_motion_hint.after(avatar::smooth_remote_avatars),
            network::send_network_intent.after(avatar::handle_avatar_movement),
            network::send_observer_update
                .after(network::apply_network_snapshot)
                .after(avatar::handle_avatar_movement),
            systems::free_camera::camera_controls.after(avatar::smooth_online_avatar_display),
            avatar::spawn_avatar,
            avatar::update_fox_animation.after(avatar::handle_avatar_movement),
            avatar::update_remote_fox_animation.after(avatar::tick_remote_avatar_motion_hint),
            systems::debug::debug_region_entities.after(rendering::spawn_regions),
        ),
    )
    // Phase 4: Prim selection, raycasting, marquee, keyboard shortcuts
    .add_systems(
        Update,
        (
            // After the gizmo so a handle-drag (sets active_axis) is seen this frame and
            // the click isn't treated as a world selection.
            systems::picking::prim_picking.after(systems::gizmo::handle_gizmo_interaction),
            systems::picking::update_marquee.after(systems::picking::prim_picking),
            systems::picking::render_marquee.after(systems::picking::update_marquee),
            systems::picking::handle_delete_key,
            systems::picking::highlight_selected_prim,
            systems::picking::unhighlight_deselected_prim,
        ),
    )
    // Phase 5: Context menu and edit dialog
    .add_systems(
        Update,
        (
            systems::ui::render_context_menu,
            // Follow selection changes from picking before the dialog draws / live edits apply.
            systems::ui::sync_dialog_to_selection
                .after(systems::picking::prim_picking)
                .before(systems::ui::render_edit_dialog),
            systems::ui::render_edit_dialog,
            systems::ui::apply_live_prim_edits.after(systems::ui::render_edit_dialog),
            systems::ui::send_prim_mutations.after(systems::ui::render_edit_dialog),
        ),
    )
    // Phase 7: AI assistant panel + top menu bar
    .add_systems(
        Update,
        (
            // Top menu bar must reserve its space before the AI side panel lays out.
            systems::ui::render_menu_bar.before(systems::ai_assistant::render_ai_panel),
            systems::ui::toggle_ai_panel_shortcut,
            systems::ui::toggle_dev_panel_shortcut,
            systems::ui::render_dev_panel,
            advance_day_cycle.before(apply_day_night_cycle),
            apply_day_night_cycle.after(systems::ui::render_dev_panel),
            apply_distance_fog.after(systems::ui::render_dev_panel),
            update_window_glow,
            // Track the camera after it has been positioned this frame.
            update_star_sky.after(systems::free_camera::camera_controls),
            update_horizon_haze.after(systems::free_camera::camera_controls),
            track_atmosphere_altitude.after(systems::free_camera::camera_controls),
            systems::ai_assistant::render_ai_panel,
            systems::ai_assistant::poll_ai_response
                .after(systems::ai_assistant::render_ai_panel),
        ),
    )
    // Phase 6: Transform gizmos
    .add_systems(
        Update,
        (
            systems::gizmo::handle_gizmo_mode_input,
            systems::gizmo::render_gizmo_visuals,
            systems::gizmo::handle_gizmo_interaction,
        ),
    );

    app.run();
}

fn has_connect_addr(addr: Option<Res<ConnectAddr>>) -> bool {
    addr.is_some()
}

fn no_connect_addr(addr: Option<Res<ConnectAddr>>) -> bool {
    addr.is_none()
}

fn has_database(db: Option<Res<Database>>) -> bool {
    db.is_some()
}

fn spawn_avatar_entity(mut commands: Commands) {
    commands.spawn((
        Avatar,
        Transform::from_translation(Vec3::new(0.0, 0.0, 0.0)).with_scale(Vec3::splat(0.02)),
    ));
}

/// Attach the avatar to the world `BigSpace` once it exists, so it rebases with
/// the camera instead of glitching past the first cell (ADR-019).
fn attach_avatar_to_bigspace(
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

fn setup_sky(
    mut commands: Commands,
    mut atmosphere: AtmosphereMut<Nishita>,
    dev: Res<DevPanelState>,
) {
    // Start from the dev panel's time of day so the sky/sun match it from frame one
    // (apply_day_night_cycle keeps them in sync afterwards).
    let sun_dir = sun_direction(dev.time_of_day);
    atmosphere.sun_position = sun_dir;
    // The Nishita default puts the viewer 1 km above the surface, which drops the
    // sky's horizon ~1° below the city skyline and exposes a dark under-horizon
    // wedge (short, in-scatter-starved rays). Park it at rooftop altitude instead.
    atmosphere.ray_origin = Vec3::new(0.0, atmosphere.planet_radius + 60.0, 0.0);
    let day = sun_dir.y.max(0.0);

    commands.spawn((
        DirectionalLight {
            illuminance: AMBIENT_DAYLIGHT * day * SUN_SCALE,
            shadows_enabled: true,
            ..default()
        },
        // Cascades sized to cover the visible city while keeping crisp near shadows.
        CascadeShadowConfigBuilder {
            num_cascades: 4,
            minimum_distance: 2.0,
            maximum_distance: 3000.0,
            first_cascade_far_bound: 60.0,
            overlap_proportion: 0.2,
        }
        .build(),
        Transform::from_translation(Vec3::ZERO).looking_to(-sun_dir, Vec3::Y),
    ));

    commands.insert_resource(AmbientLight {
        color: Color::WHITE,
        brightness: 10.0 + AMBIENT_DAY_BRIGHTNESS * day,
        affects_lightmapped_meshes: true,
    });
}

/// Global trim on the sun's illuminance: without tonemapping, full daylight on the
/// near-white OSM basemap clips to solid white and reads oversaturated.
const SUN_SCALE: f32 = 0.8;
/// Peak added ambient brightness at noon (same clipping consideration).
const AMBIENT_DAY_BRIGHTNESS: f32 = 2400.0;

/// Marker for the world-aligned night-sky (star map) sphere.
#[derive(Component)]
struct StarSky;

/// Brightness multiplier for the (fairly dim) star map so stars/Milky Way read clearly.
const STAR_BRIGHTNESS: f32 = 3.0;

/// Material that maps an equirectangular star map onto the surrounding sphere by view
/// direction (see `shaders/starsky.wgsl`) — no UV-sphere poles or seams. Additive, with
/// `brightness` driven by the night factor.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct StarSkyMaterial {
    #[uniform(0)]
    brightness: f32,
    #[texture(1)]
    #[sampler(2)]
    texture: Handle<Image>,
}

impl Material for StarSkyMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/starsky.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Add
    }

    fn specialize(
        _pipeline: &MaterialPipeline<Self>,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        use bevy::render::render_resource::{
            BlendComponent, BlendFactor, BlendOperation, BlendState,
        };
        // Camera is inside the sphere; render its interior.
        descriptor.primitive.cull_mode = None;
        // Don't write depth — it must not occlude the atmosphere or anything else.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = false;
        }
        // Force pure additive blending (src + dst): black gaps add nothing, stars add light.
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.get_mut(0) {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::Zero,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
}

/// Marker for the fog-colored horizon-haze shell (see `shaders/horizonhaze.wgsl`).
#[derive(Component)]
struct HorizonHaze;

/// Alpha-blended band of fog color around the horizon, bridging the fogged ground
/// and the atmosphere sky (whose single-scattering model darkens at grazing angles).
/// Colored from the camera's `DistanceFog` each frame (see `update_horizon_haze`).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct HorizonHazeMaterial {
    #[uniform(0)]
    color: LinearRgba,
    /// x: elevation where the haze fades out fully, y: peak alpha,
    /// z: directional-light exponent (w unused).
    #[uniform(1)]
    params: Vec4,
    /// `DistanceFog::directional_light_color` — rgb tint, alpha = strength.
    #[uniform(2)]
    dir_light_color: LinearRgba,
}

impl Material for HorizonHazeMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/horizonhaze.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline<Self>,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Camera is inside the shell; render its interior, and don't occlude
        // anything behind it (depth test still lets world geometry cover it).
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = false;
        }
        Ok(())
    }
}

/// When the dev panel's "Quick cycle" toggle is on, advances the time of day smoothly,
/// looping the full 24 h over `cycle_seconds`.
fn advance_day_cycle(time: Res<Time>, mut dev: ResMut<DevPanelState>) {
    if !dev.auto_cycle {
        return;
    }
    let speed = 24.0 / dev.cycle_seconds.max(0.1); // hours per real second
    dev.time_of_day = (dev.time_of_day + time.delta_secs() * speed).rem_euclid(24.0);
}

/// Drives the sun direction, sun light intensity, and ambient brightness from the dev
/// panel's time-of-day slider (Ctrl+Shift+D). Runs only when the value changes.
fn apply_day_night_cycle(
    dev: Res<DevPanelState>,
    mut atmosphere: AtmosphereMut<Nishita>,
    mut sun: Query<(&mut Transform, &mut DirectionalLight)>,
    mut ambient: ResMut<AmbientLight>,
) {
    if !dev.is_changed() {
        return;
    }
    let sun_dir = sun_direction(dev.time_of_day);
    atmosphere.sun_position = sun_dir;

    let day = sun_dir.y.max(0.0); // 0 at/below the horizon, → 1 overhead
    // Fog diffuses sunlight: dim the direct sun (softening shadows) as density rises.
    let sun_through_fog = 1.0 - 0.75 * dev.fog_density;
    for (mut tf, mut light) in sun.iter_mut() {
        *tf = Transform::from_translation(Vec3::ZERO).looking_to(-sun_dir, Vec3::Y);
        light.illuminance = AMBIENT_DAYLIGHT * day * SUN_SCALE * sun_through_fog;
    }
    ambient.brightness = 10.0 + AMBIENT_DAY_BRIGHTNESS * day;
}

/// Drives the camera's atmospheric [`DistanceFog`] from the dev panel's fog slider
/// and the time of day (fog darkens at night). Also couples the sky to the fog:
/// fog is Mie scattering, so raising the atmosphere's Mie coefficient with density
/// turns the sky itself milky and whites out the horizon instead of leaving crisp
/// blue sky above the fog wall. Runs only when the dev panel changes.
fn apply_distance_fog(
    dev: Res<DevPanelState>,
    mut fog: Query<&mut DistanceFog, With<systems::free_camera::FreeCamera>>,
) {
    if !dev.is_changed() {
        return;
    }
    // The atmosphere itself stays a clear sky: boosting Nishita's Mie scattering
    // with density was tried and abandoned — single scattering *darkens* the low
    // sky instead of whitening it. The horizon-haze shell fogs the sky instead
    // (see update_horizon_haze), veiling the whole dome as density rises.
    let elevation = sun_direction(dev.time_of_day).y;
    for mut f in fog.iter_mut() {
        *f = systems::free_camera::distance_fog(dev.fog_density, elevation);
    }
}

/// Keeps the Nishita atmosphere's viewpoint at the camera's actual altitude so the
/// sky's horizon line stays correct when flying up. Quantized (25 m) because every
/// model change re-renders the sky cubemap.
fn track_atmosphere_altitude(
    camera: Query<(&GridCell, &Transform), With<systems::free_camera::FreeCamera>>,
    mut atmosphere: AtmosphereMut<Nishita>,
    mut last_altitude: Local<f32>,
) {
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let altitude =
        (cell.y as f32 * systems::free_camera::WORLD_CELL_EDGE + tf.translation.y).max(10.0);
    if *last_altitude == 0.0 || (altitude - *last_altitude).abs() > 25.0 {
        *last_altitude = altitude;
        atmosphere.ray_origin.y = atmosphere.planet_radius + altitude;
    }
}

/// Spawns the star-map night sky: a box centred on the viewer whose custom material
/// samples the equirectangular star map by view direction (no UV-sphere poles or seams).
/// Fade the emissive building windows in as the sun drops below the horizon, so the
/// city lights up at night (and windows read as dark glass by day).
fn update_window_glow(
    dev: Res<DevPanelState>,
    windows: Query<(
        &systems::osm_buildings::WindowGlow,
        &MeshMaterial3d<StandardMaterial>,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let elevation = sun_direction(dev.time_of_day).y;
    // Begin glowing as the sun lowers in the evening (elevation ~0.18), reaching
    // full a little after dusk — a gentle ramp, not an on/off at full dark.
    let glow = ((0.18 - elevation) / 0.33).clamp(0.0, 1.0);
    for (bucket, mat) in windows.iter() {
        if let Some(m) = materials.get_mut(&mat.0) {
            let e = 1.2 * glow * bucket.0; // subtle, warm; per-bucket brightness
            m.emissive = LinearRgba::rgb(e, 0.72 * e, 0.34 * e);
        }
    }
}

/// `update_star_sky` keeps it on the camera and fades it with the night factor.
fn setup_stars(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StarSkyMaterial>>,
    asset_server: Res<AssetServer>,
) {
    // Repeat addressing so the longitude seam (where the sampled U wraps 1→0 behind the
    // viewer) is continuous. The mesh UVs are unused — the shader samples by direction —
    // so a low-poly sphere is fine; it only needs to enclose the camera.
    let texture = asset_server.load_with_settings(
        "sky/stars_milky_way_8k.jpg",
        |s: &mut bevy::image::ImageLoaderSettings| {
            s.sampler = crate::systems::rendering::repeat_linear_sampler();
        },
    );
    // A box just inside the atmosphere's skybox cube (faces at ~0.707 × SKYBOX_MESH_FAR,
    // i.e. ~212 km) — a smaller concentric cube is uniformly in front of it in every
    // direction, so no part pokes through, while staying beyond all world geometry so
    // stars never draw over distant buildings. The shader samples by view direction, so
    // the box shape is irrelevant to the mapping; it's just a cheap 12-triangle shell.
    let mesh = meshes.add(Cuboid::from_length(400_000.0).mesh());
    let material = materials.add(StarSkyMaterial {
        brightness: 0.0, // set each frame from the night factor; starts invisible (day)
        texture,
    });
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        bevy::pbr::NotShadowCaster,
        bevy::pbr::NotShadowReceiver,
        StarSky,
    ));
}

/// Spawns the fog-colored horizon-haze shell: a slightly smaller sibling of the star
/// box whose shader paints a band of fog color at the horizon (see horizonhaze.wgsl).
fn setup_horizon_haze(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<HorizonHazeMaterial>>,
) {
    let mesh = meshes.add(Cuboid::from_length(360_000.0).mesh());
    let material = materials.add(HorizonHazeMaterial {
        color: LinearRgba::WHITE,
        params: Vec4::ZERO, // set each frame from the distance fog
        dir_light_color: LinearRgba::NONE,
    });
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        bevy::pbr::NotShadowCaster,
        bevy::pbr::NotShadowReceiver,
        HorizonHaze,
    ));
}

/// Keeps the horizon-haze shell on the camera and matches its color and strength to
/// the camera's `DistanceFog`, so fogged ground and sky meet seamlessly at the horizon.
fn update_horizon_haze(
    dev: Res<DevPanelState>,
    camera: Query<(&GlobalTransform, &DistanceFog), With<Camera3d>>,
    mut haze: Query<(&mut Transform, &MeshMaterial3d<HorizonHazeMaterial>), With<HorizonHaze>>,
    mut materials: ResMut<Assets<HorizonHazeMaterial>>,
) {
    let Ok((cam, fog)) = camera.single() else {
        return;
    };
    let Ok((mut tf, mat_handle)) = haze.single_mut() else {
        return;
    };
    tf.translation = cam.translation();

    if let Some(mat) = materials.get_mut(&mat_handle.0) {
        let d = dev.fog_density;
        mat.color = fog.color.to_linear();
        // The band rises and strengthens with fog density; a small base keeps the
        // sky's single-scattering horizon artifact covered even with the fog off.
        // The zenith floor (w) grows quadratically so light haze leaves the sky
        // blue while dense fog closes over the whole dome.
        mat.params = Vec4::new(
            0.03 + 0.25 * d,
            (0.35 + 1.2 * d).min(1.0),
            fog.directional_light_exponent,
            d * d,
        );
        mat.dir_light_color = fog.directional_light_color.to_linear();
    }
}

/// Keeps the star box centred on the camera (so it reads as infinitely far) and fades
/// it in after sunset by driving the material's `brightness` from the night factor.
fn update_star_sky(
    dev: Res<DevPanelState>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
    mut star: Query<(&mut Transform, &MeshMaterial3d<StarSkyMaterial>), With<StarSky>>,
    mut materials: ResMut<Assets<StarSkyMaterial>>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let Ok((mut tf, mat_handle)) = star.single_mut() else {
        return;
    };
    tf.translation = cam.translation();

    // Fade in once the sun is below the horizon; full when it's well below (dark sky).
    let elevation = sun_direction(dev.time_of_day).y;
    let night = ((-0.05 - elevation) / 0.2).clamp(0.0, 1.0);
    if let Some(mat) = materials.get_mut(&mat_handle.0) {
        mat.brightness = night * STAR_BRIGHTNESS;
    }
}
