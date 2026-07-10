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
    .add_plugins(MaterialPlugin::<StarFieldMaterial>::default())
    .add_plugins(MaterialPlugin::<MoonMaterial>::default())
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
            setup_moon,
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
            update_moon.after(systems::free_camera::camera_controls),
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
    let (sun_color, illuminance) = sun_light(dev.time_of_day, dev.fog_density);

    commands.spawn((
        DirectionalLight {
            color: sun_color,
            illuminance,
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
        SunLight,
    ));

    // Moonlight: a second, faint directional light (kept in sync with the moon
    // disc by apply_day_night_cycle). Perceptually blue-gray, with a shorter
    // shadow range than the sun — moon shadows only need to read up close.
    let moon_dir = utils::sun::moon_direction(dev.time_of_day, dev.moon_age_days);
    commands.spawn((
        DirectionalLight {
            color: Color::linear_rgb(0.55, 0.72, 1.0),
            illuminance: MOONLIGHT_ILLUMINANCE
                * moonlight_factor(dev.time_of_day, dev.moon_age_days),
            shadows_enabled: true,
            ..default()
        },
        CascadeShadowConfigBuilder {
            num_cascades: 2,
            minimum_distance: 2.0,
            maximum_distance: 1200.0,
            first_cascade_far_bound: 80.0,
            overlap_proportion: 0.2,
        }
        .build(),
        Transform::from_translation(Vec3::ZERO).looking_to(-moon_dir, Vec3::Y),
        MoonLight,
    ));

    commands.insert_resource(ambient_light(dev.time_of_day, dev.moon_age_days));
}

/// Global trim on the sun's illuminance: the OSM basemap is near-white, so full
/// physical daylight drives most of the frame into the tonemapper's shoulder
/// (Bevy's default TonyMcMapface) and everything reads flat and washed out.
const SUN_SCALE: f32 = 0.75;
/// Peak added ambient brightness at noon (same washout consideration — and darker
/// ambient keeps midday shadows readable, which is where the depth comes from).
const AMBIENT_DAY_BRIGHTNESS: f32 = 1800.0;
/// Ambient floor at night — faint blue city skyglow so the world stays legible
/// instead of vanishing into black between the lit windows.
const AMBIENT_NIGHT_BRIGHTNESS: f32 = 80.0;
/// Peak moonlight illuminance (full moon at the zenith), on the same trimmed
/// scale as the sun. Deliberately well above the night ambient floor: for the
/// directional shaping to *read* — lit roofs and moon-facing facades standing
/// out, soft shadows behind buildings — moonlight must dominate the ambient,
/// which is also how a real moonlit night looks (the full moon is ~2500× any
/// other natural night-sky light).
const MOONLIGHT_ILLUMINANCE: f32 = 180.0;
/// Peak added ambient from a full moon overhead — the sky-scattered part.
const MOON_AMBIENT_BRIGHTNESS: f32 = 80.0;

/// Marker for the sun's directional light.
#[derive(Component)]
struct SunLight;

/// Marker for the moon's directional light.
#[derive(Component)]
struct MoonLight;

/// Moonlight strength 0–1: moon elevation × illuminated fraction, faded out by
/// day (moonlight is negligible next to any amount of daylight).
fn moonlight_factor(hours: f32, moon_age_days: f32) -> f32 {
    let up = utils::sun::moon_direction(hours, moon_age_days).y.max(0.0);
    up * utils::sun::moon_illuminated_fraction(moon_age_days)
        * (1.0 - utils::sun::daylight(hours))
}

/// Sun light color and illuminance for a time of day. The hue and magnitude both
/// come from the atmosphere's transmittance toward the sun (see `utils::sun`), so
/// the light warms and dims exactly as the sky it hangs in: warm white at noon,
/// gold in the morning/evening, deep red and faint as the disc grazes the horizon.
fn sun_light(hours: f32, fog_density: f32) -> (Color, f32) {
    let t = utils::sun::sun_transmittance(hours);
    let peak = t.max_element().max(1e-4);
    // Hue in the color (peak-normalized), magnitude in the illuminance.
    let color = Color::linear_rgb(t.x / peak, t.y / peak, t.z / peak);
    // Fog diffuses sunlight: dim the direct sun (softening shadows) as density rises.
    let sun_through_fog = 1.0 - 0.75 * fog_density;
    let illuminance =
        AMBIENT_DAYLIGHT * SUN_SCALE * peak * utils::sun::sun_disc(hours) * sun_through_fog;
    (color, illuminance)
}

/// Ambient light for a time of day: brightness follows the sky through twilight
/// (the sky lights the city before sunrise and after sunset), color follows the
/// sky's hue — blue night, warm dawn/dusk, cool-white day.
fn ambient_light(hours: f32, moon_age_days: f32) -> AmbientLight {
    let c = utils::sun::ambient_sky_color(hours);
    AmbientLight {
        color: Color::linear_rgb(c.x, c.y, c.z),
        brightness: AMBIENT_NIGHT_BRIGHTNESS
            + AMBIENT_DAY_BRIGHTNESS * utils::sun::daylight(hours)
            + MOON_AMBIENT_BRIGHTNESS * moonlight_factor(hours, moon_age_days),
        affects_lightmapped_meshes: true,
    }
}

/// Marker for the catalog star field (spun around the celestial pole).
#[derive(Component)]
struct StarSky;

/// Global brightness multiplier for the star field.
const STAR_BRIGHTNESS: f32 = 3.5;

/// Star-shell radius: beyond all world geometry (so buildings and the ground
/// occlude stars) but inside the far plane and the atmosphere skybox faces.
const STAR_SHELL_RADIUS: f32 = 150_000.0;

/// Material for the catalog star field (see `shaders/starfield.wgsl`): additive
/// gaussian points, with per-star tint and twinkle seed in the vertex colors.
/// `params.x` = overall brightness (night factor × fog veil), `.y` = twinkle clock.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct StarFieldMaterial {
    #[uniform(0)]
    params: Vec4,
}

impl Material for StarFieldMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/starfield.wgsl".into()
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
        // Quad winding depends on each star's tangent basis; draw both faces.
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

/// Marker for the moon disc quad.
#[derive(Component)]
struct Moon;

/// Angular radius of the moon *quad* in radians. The disc proper fills the inner
/// ~42% of it (`DISC_R` in moon.wgsl) — ~0.7°, ~2.5× the real moon, which is how
/// cameras exaggerate it too; the rest of the quad carries the radiant glow halo.
const MOON_ANGULAR_RADIUS: f32 = 0.028;
/// Moon shell distance: inside the star shell so the disc occludes stars behind it.
const MOON_DISTANCE: f32 = 140_000.0;

/// Material for the moon disc (see `shaders/moon.wgsl`): a sphere-shaded quad lit
/// by the sun direction expressed in the quad's local frame (xyz), so the lunar
/// phase and the terminator's orientation emerge from the real sun–moon geometry.
/// `sun_local.w` = overall brightness; alpha-blended so the disc covers stars.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct MoonMaterial {
    #[uniform(0)]
    sun_local: Vec4,
}

impl Material for MoonMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/moon.wgsl".into()
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
        descriptor.primitive.cull_mode = None;
        // Don't write depth — it must not occlude the atmosphere or the stars' pass.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = false;
        }
        Ok(())
    }
}

/// Spawns the moon disc: a single quad in its local XY plane with UVs spanning
/// [-1, 1]²; `update_moon` points it at the camera and shades it via the material.
fn setup_moon(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<MoonMaterial>>,
) {
    let s = MOON_DISTANCE * MOON_ANGULAR_RADIUS;
    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        bevy::render::render_asset::RenderAssetUsages::RENDER_WORLD,
    );
    let corners = [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        corners
            .iter()
            .map(|&(u, v)| [u * s, v * s, 0.0])
            .collect::<Vec<_>>(),
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0, 0.0, 1.0]; 4],
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        corners.iter().map(|&(u, v)| [u, v]).collect::<Vec<_>>(),
    );
    mesh.insert_indices(bevy::render::mesh::Indices::U32(vec![0, 1, 2, 0, 2, 3]));

    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(materials.add(MoonMaterial {
            sun_local: Vec4::ZERO, // set each frame; starts invisible
        })),
        Transform::default(),
        bevy::pbr::NotShadowCaster,
        bevy::pbr::NotShadowReceiver,
        Moon,
    ));
}

/// Places the moon disc on the sky shell around the camera and feeds the material
/// the sun direction in the quad's local frame (phase shading) plus the overall
/// brightness: fading in through dusk, veiled by fog, extinguished near the horizon.
fn update_moon(
    dev: Res<DevPanelState>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
    mut moon: Query<(&mut Transform, &MeshMaterial3d<MoonMaterial>), With<Moon>>,
    mut materials: ResMut<Assets<MoonMaterial>>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let Ok((mut tf, mat_handle)) = moon.single_mut() else {
        return;
    };
    let moon_dir = utils::sun::moon_direction(dev.time_of_day, dev.moon_age_days);
    let rotation = Quat::from_rotation_arc(Vec3::Z, -moon_dir);
    tf.translation = cam.translation() + moon_dir * MOON_DISTANCE;
    tf.rotation = rotation;

    // Visible from twilight on (the moon shows before full dark), dimmed by fog
    // and by atmospheric extinction as it nears the horizon.
    let sun_elev = sun_direction(dev.time_of_day).y;
    let dusk = ((0.05 - sun_elev) / 0.15).clamp(0.0, 1.0);
    let fog_veil = (-3.0 * dev.fog_density).exp();
    let extinction = 0.3 + 0.7 * utils::sun::smoothstep(0.0, 0.25, moon_dir.y);
    let brightness = dusk * fog_veil * extinction;

    if let Some(mat) = materials.get_mut(&mat_handle.0) {
        // Phase-consistent sunlight (not the raw sun direction — see sun.rs).
        let sun_local = rotation.inverse()
            * utils::sun::moon_sunlight_direction(dev.time_of_day, dev.moon_age_days);
        mat.sun_local = sun_local.extend(brightness);
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
    let dh = time.delta_secs() * speed;
    dev.time_of_day = (dev.time_of_day + dh).rem_euclid(24.0);
    // The moon ages with the cycling days, so moonrise drifts ~50 min later per
    // day and the phase creeps through the month, just like the real moon.
    dev.moon_age_days =
        (dev.moon_age_days + dh / 24.0).rem_euclid(utils::sun::LUNAR_CYCLE_DAYS);
}

/// Drives the sun direction, sun light intensity, and ambient brightness from the dev
/// panel's time-of-day slider (Ctrl+Shift+D). Runs only when the value changes.
fn apply_day_night_cycle(
    dev: Res<DevPanelState>,
    mut atmosphere: AtmosphereMut<Nishita>,
    mut sun: Query<(&mut Transform, &mut DirectionalLight), (With<SunLight>, Without<MoonLight>)>,
    mut moon: Query<(&mut Transform, &mut DirectionalLight), With<MoonLight>>,
    mut ambient: ResMut<AmbientLight>,
) {
    if !dev.is_changed() {
        return;
    }
    let sun_dir = sun_direction(dev.time_of_day);
    atmosphere.sun_position = sun_dir;

    let (sun_color, illuminance) = sun_light(dev.time_of_day, dev.fog_density);
    for (mut tf, mut light) in sun.iter_mut() {
        *tf = Transform::from_translation(Vec3::ZERO).looking_to(-sun_dir, Vec3::Y);
        light.color = sun_color;
        light.illuminance = illuminance;
    }

    // Moonlight tracks the moon disc; fog diffuses it like it does the sun.
    let moon_dir = utils::sun::moon_direction(dev.time_of_day, dev.moon_age_days);
    let moonlight = MOONLIGHT_ILLUMINANCE
        * moonlight_factor(dev.time_of_day, dev.moon_age_days)
        * (1.0 - 0.75 * dev.fog_density);
    for (mut tf, mut light) in moon.iter_mut() {
        *tf = Transform::from_translation(Vec3::ZERO).looking_to(-moon_dir, Vec3::Y);
        light.illuminance = moonlight;
        // Shadow passes aren't free — only run them while moonlight is visible.
        light.shadows_enabled = moonlight > 1.0;
    }

    *ambient = ambient_light(dev.time_of_day, dev.moon_age_days);
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
    for mut f in fog.iter_mut() {
        *f = systems::free_camera::distance_fog(dev.fog_density, dev.time_of_day);
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

/// Builds the star field from the embedded bright-star catalog (~9k naked-eye
/// stars, HYG database — see `tools/gen_stars.py`): one tiny sphere-tangent quad
/// per star at its real equatorial position, weighted by magnitude and tinted by
/// B−V color temperature, with a per-star twinkle seed in the vertex alpha. The
/// whole sky is one mesh and one draw call; `update_star_sky` spins it around the
/// celestial pole, fades it with the night, and drives the twinkle clock.
fn setup_stars(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StarFieldMaterial>>,
) {
    static CATALOG: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/sky/bright_stars.bin"
    ));
    let n = CATALOG.len() / 16;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * 4);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(n * 4);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(n * 4);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(n * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(n * 6);

    for (i, rec) in CATALOG.chunks_exact(16).enumerate() {
        let f = |k: usize| f32::from_le_bytes(rec[4 * k..4 * k + 4].try_into().unwrap());
        let (ra, dec, mag, bv) = (f(0), f(1), f(2), f(3));

        // Equatorial frame with +Y at the north celestial pole. RA winds so that a
        // larger right ascension culminates *later* under the westward diurnal spin,
        // which keeps constellations un-mirrored for a ground observer.
        let dir = Vec3::new(dec.cos() * ra.cos(), dec.sin(), -(dec.cos() * ra.sin()));
        // Tangent basis for the quad (X fallback for stars at the poles).
        let up = if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y };
        let t1 = up.cross(dir).normalize();
        let t2 = dir.cross(t1);

        // Compressed magnitude scale: real flux spans ~1500:1 from Sirius to the
        // naked-eye limit, far more than additive blending can usefully show.
        let lum = 10f32.powf(-0.24 * mag).min(2.5);
        // Bright stars also get a slightly larger quad (their glow bleeds wider).
        let half_angle = 0.0010 + 0.0009 * ((2.5 - mag) / 4.0).clamp(0.0, 1.0);
        let tint = star_color(bv) * lum;
        let seed = (i as f32 * 0.618_034).fract(); // per-star twinkle phase

        let base = positions.len() as u32;
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = (dir + (t1 * su + t2 * sv) * half_angle) * STAR_SHELL_RADIUS;
            positions.push([p.x, p.y, p.z]);
            normals.push([dir.x, dir.y, dir.z]);
            uvs.push([su, sv]);
            colors.push([tint.x, tint.y, tint.z, seed]);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        bevy::render::render_asset::RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(bevy::render::mesh::Indices::U32(indices));

    let material = materials.add(StarFieldMaterial {
        params: Vec4::ZERO, // brightness set each frame; starts invisible (day)
    });
    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(material),
        Transform::default(),
        bevy::pbr::NotShadowCaster,
        bevy::pbr::NotShadowReceiver,
        StarSky,
    ));
}

/// Approximate display tint for a stellar B−V color index: blue-white for hot
/// stars (Rigel, −0.3) through white and yellow to orange-red for cool ones
/// (Betelgeuse, ~1.85). Piecewise-linear over perceptual anchor stops.
fn star_color(bv: f32) -> Vec3 {
    const STOPS: [(f32, Vec3); 6] = [
        (-0.3, Vec3::new(0.62, 0.72, 1.0)),
        (0.0, Vec3::new(0.79, 0.86, 1.0)),
        (0.4, Vec3::new(1.0, 0.98, 0.92)),
        (0.8, Vec3::new(1.0, 0.90, 0.76)),
        (1.5, Vec3::new(1.0, 0.80, 0.60)),
        (2.0, Vec3::new(1.0, 0.70, 0.46)),
    ];
    let bv = bv.clamp(STOPS[0].0, STOPS[STOPS.len() - 1].0);
    for w in STOPS.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if bv <= b {
            return ca.lerp(cb, (bv - a) / (b - a));
        }
    }
    STOPS[STOPS.len() - 1].1
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

/// Keeps the star field centred on the camera (so it reads as infinitely far),
/// spins it around the celestial pole with the time of day, and drives the
/// material's brightness (night factor, veiled by fog) and twinkle clock.
fn update_star_sky(
    dev: Res<DevPanelState>,
    time: Res<Time>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
    mut star: Query<(&mut Transform, &MeshMaterial3d<StarFieldMaterial>), With<StarSky>>,
    mut materials: ResMut<Assets<StarFieldMaterial>>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let Ok((mut tf, mat_handle)) = star.single_mut() else {
        return;
    };
    tf.translation = cam.translation();
    // Diurnal rotation of the celestial sphere (shared with the moon, so they
    // move together). Quick-cycle mode becomes a star-trail time-lapse.
    tf.rotation = utils::sun::celestial_rotation(dev.time_of_day);

    // Fade in once the sun is below the horizon; full when it's well below (dark sky).
    let elevation = sun_direction(dev.time_of_day).y;
    let night = ((-0.05 - elevation) / 0.2).clamp(0.0, 1.0);
    // Fog is opaque to starlight long before it swallows the city: veil the star
    // field with density so a foggy night reads as murk, not crisp stars over murk.
    let fog_veil = (-3.0 * dev.fog_density).exp();
    if let Some(mat) = materials.get_mut(&mat_handle.0) {
        mat.params = Vec4::new(
            night * STAR_BRIGHTNESS * fog_veil,
            time.elapsed_secs(),
            0.0,
            0.0,
        );
    }
}
