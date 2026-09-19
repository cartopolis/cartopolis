//! `carto-view` — look at a vector tile's ground, fast.
//!
//! Reads the **same tiles the client reads** and runs the **same tessellation the
//! client runs** (`cartopolis_geo`), then draws the result with stock materials. No
//! streaming, no height survey, no simulation, no sign-in, no user interface: what
//! it shows is a function of the tiles and the camera, and of nothing that happened
//! to finish arriving.
//!
//! ```bash
//! carto-view --tile-dir /tmp/tiles --cells 14/8489/5321 --shot /tmp/ground.png
//! ```
//!
//! # Why it is not the client
//!
//! Looking at a change to the ground meant launching the whole client: a sign-in
//! path, a tile streamer, an egui stack, and a capture that waits for all of it to
//! go quiet — six minutes a viewpoint on a software renderer, and a picture that
//! depended on what had arrived. This is seconds, and the same input gives the
//! same picture every time.
//!
//! It deliberately carries **none of the client's rendering** — no custom
//! materials, no depth-bias pass, no atmosphere, and shadows off, because a dark
//! band across a surface is what a ground fault looks like too. What it shares is
//! `cartopolis_geo`: the tessellation, the surface list and the depth ladder. That
//! is what the tiles *mean*, not how they are drawn.
//!
//! Headless by default — the machines this runs on have no display. `--window`
//! asks for one where there is something to put it on.

use anyhow::{Context, Result};
use bevy::asset::RenderAssetUsages;
// `RenderTarget` is its own component in Bevy 0.19 (required by `Camera`), and it
// lives in `bevy::camera` rather than the prelude.
use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use clap::Parser;

use cartopolis_geo::road_texture::RoadClass;
use cartopolis_geo::vector_tiles::{self, SurfaceGroup, SurfaceMesh, TileData};

#[derive(Parser, Resource, Clone)]
#[command(
    name = "carto-view",
    about = "Draw a vector tile's ground, without launching the client."
)]
struct Cli {
    /// Tiles on disk, as `{z}/{x}/{y}.mvt` under this directory.
    #[arg(long, value_name = "DIR")]
    tile_dir: std::path::PathBuf,

    /// Which cells to draw, as `z/x/y`. Repeatable.
    #[arg(long, value_name = "Z/X/Y", required = true, value_parser = parse_cell)]
    cells: Vec<(u32, i64, i64)>,

    /// Write one frame here and exit.
    #[arg(long, value_name = "PATH")]
    shot: Option<std::path::PathBuf>,

    /// Camera pitch and yaw in degrees. Note the `=`: clap reads a leading `-35`
    /// as a flag otherwise.
    #[arg(long, value_name = "PITCH,YAW", default_value = "-35,45", value_parser = parse_look)]
    look: (f32, f32),

    /// Stand the camera at `lat,lng,altitude` and look along `--look`. The same
    /// convention `docs/shots.toml` uses, so a viewpoint found once works in
    /// either instrument.
    #[arg(long, value_name = "LAT,LNG,ALT", value_parser = parse_at)]
    at: Option<(f64, f64, f32)>,

    /// Metres back from the middle of what was loaded, when `--at` is not given.
    #[arg(long, value_name = "METRES")]
    distance: Option<f32>,

    /// Offscreen size, `WxH`.
    #[arg(long, value_name = "WxH", default_value = "1200x900", value_parser = parse_size)]
    size: (u32, u32),

    /// Open a window instead of rendering offscreen. Needs a display.
    #[arg(long)]
    window: bool,

    /// Camera exposure as a photographic EV100. It decides whether ground *shape*
    /// is visible at all: over-expose and every surface clips to white, taking the
    /// shading with it. 15 is daylight and pairs with the physical sunlight below.
    #[arg(long, value_name = "EV100", default_value_t = 15.0)]
    exposure: f32,

    /// Metres below the ground to lay the checkered underlay.
    #[arg(long, value_name = "METRES", default_value_t = 30.0)]
    under: f32,

    /// Draw only the surface groups whose name contains this, case-insensitively —
    /// `--only road`, `--only water`.
    ///
    /// **The diagnostic for "is it there at all".** Reading presence off a picture
    /// of everything is how a surface gets called missing when it is merely behind
    /// something, or called present when the thing you are looking at is its
    /// neighbour.
    #[arg(long, value_name = "SUBSTRING")]
    only: Option<String>,

    /// Draw every surface from both sides — the diagnostic for "missing or facing
    /// away". A ground surface wound the wrong way round is invisible from above
    /// and identical to one that was never built.
    #[arg(long)]
    double_sided: bool,

    /// Draw every surface in one tone, so the *shape* of the ground is what you
    /// see rather than the map's colours.
    #[arg(long)]
    plain: bool,
}

fn parse_cell(s: &str) -> Result<(u32, i64, i64), String> {
    let p: Vec<&str> = s.split('/').collect();
    if p.len() != 3 {
        return Err("expected Z/X/Y".into());
    }
    Ok((
        p[0].trim().parse().map_err(|_| "zoom is not a number")?,
        p[1].trim().parse().map_err(|_| "x is not a number")?,
        p[2].trim().parse().map_err(|_| "y is not a number")?,
    ))
}

fn parse_look(s: &str) -> Result<(f32, f32), String> {
    let (p, y) = s.split_once(',').ok_or("expected PITCH,YAW")?;
    Ok((
        p.trim().parse().map_err(|_| "pitch is not a number")?,
        y.trim().parse().map_err(|_| "yaw is not a number")?,
    ))
}

fn parse_at(s: &str) -> Result<(f64, f64, f32), String> {
    let p: Vec<&str> = s.split(',').collect();
    if p.len() != 3 {
        return Err("expected LAT,LNG,ALT".into());
    }
    let lat: f64 = p[0]
        .trim()
        .parse()
        .map_err(|_| "latitude is not a number")?;
    let lng: f64 = p[1]
        .trim()
        .parse()
        .map_err(|_| "longitude is not a number")?;
    let alt: f32 = p[2]
        .trim()
        .parse()
        .map_err(|_| "altitude is not a number")?;
    if !(-85.05..=85.05).contains(&lat) {
        return Err("latitude is outside Web Mercator's -85.05..85.05".into());
    }
    Ok((lat, lng, alt))
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once(['x', 'X']).ok_or("expected WxH")?;
    let w: u32 = w.trim().parse().map_err(|_| "width is not a number")?;
    let h: u32 = h.trim().parse().map_err(|_| "height is not a number")?;
    if w == 0 || h == 0 {
        return Err("a zero-sized image has nothing in it".into());
    }
    Ok((w, h))
}

/// One cell's tessellated ground, and where it sits.
struct Cell {
    z: u32,
    x: i64,
    y: i64,
    tile_m: f32,
    meshes: Vec<SurfaceMesh>,
}

#[derive(Resource)]
struct Loaded {
    cells: Vec<Cell>,
    /// The cell everything is placed relative to, so vertex positions stay near
    /// the origin: a tile index is five digits and a cell is ~1.5 km, so absolute
    /// tile metres would spend the whole of `f32`'s precision getting there.
    origin: (i64, i64),
}

impl Loaded {
    /// Ground metres across the loaded cells, corner to corner.
    fn extent(&self) -> f32 {
        let tile_m = self.cells.first().map_or(1465.0, |c| c.tile_m);
        let xs = self.cells.iter().map(|c| c.x);
        let ys = self.cells.iter().map(|c| c.y);
        let wide = xs.clone().max().unwrap_or(0) - xs.min().unwrap_or(0) + 1;
        let tall = ys.clone().max().unwrap_or(0) - ys.min().unwrap_or(0) + 1;
        #[allow(clippy::cast_precision_loss)]
        {
            wide.max(tall) as f32 * tile_m
        }
    }
}

#[derive(Resource, Default)]
struct Drawn {
    meshes: usize,
    triangles: usize,
}

#[derive(Resource)]
struct ShotTarget(Handle<Image>);

fn main() -> Result<()> {
    let cli = Cli::parse();
    let loaded = load(&cli).context("reading tiles")?;
    anyhow::ensure!(!loaded.cells.is_empty(), "no cells could be read");

    let mut app = App::new();
    if cli.window {
        app.add_plugins(DefaultPlugins);
    } else {
        // No window is created at all: `WinitPlugin` is never added and the runner
        // drives the frames instead.
        app.add_plugins(
            DefaultPlugins
                .build()
                .disable::<bevy::winit::WinitPlugin>()
                .set(bevy::render::RenderPlugin {
                    synchronous_pipeline_compilation: true,
                    ..default()
                }),
        )
        .add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
            std::time::Duration::ZERO,
        ));
    }

    app.insert_resource(cli)
        .insert_resource(loaded)
        .init_resource::<Drawn>()
        .insert_resource(ClearColor(Color::srgb(0.53, 0.68, 0.82)))
        .add_systems(
            Startup,
            (spawn_scene, spawn_camera, spawn_sun, spawn_checker),
        )
        .add_systems(Update, capture_when_ready);
    app.run();
    Ok(())
}

/// Read each cell's tile and tessellate it, through the client's own functions and
/// in the client's own order.
///
/// A cell that cannot be read is reported and skipped: the reason for looking is
/// usually that something is wrong, and refusing to start over one bad tile hides
/// every other one.
fn load(cli: &Cli) -> Result<Loaded> {
    let mut cells = Vec::new();
    for &(z, x, y) in &cli.cells {
        let path = cli.tile_dir.join(format!("{z}/{x}/{y}.mvt"));
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipped");
                continue;
            }
        };
        let tile = match TileData::parse(&bytes) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "unreadable tile");
                continue;
            }
        };
        #[allow(clippy::cast_possible_truncation)]
        let tile_m = {
            let (lat, _) = cartopolis_core::world::tile_to_lat_lng(x, y, z);
            cartopolis_core::world::tile_to_meters(z, lat) as f32
        };
        match tessellate(&tile, tile_m) {
            Ok(meshes) => cells.push(Cell {
                z,
                x,
                y,
                tile_m,
                meshes,
            }),
            Err(e) => tracing::warn!(path = %path.display(), error = %format!("{e:#}"), "failed"),
        }
    }
    let origin = cells.first().map_or((0, 0), |c| (c.x, c.y));
    Ok(Loaded { cells, origin })
}

/// The ground, in the same order `systems::map_geometry` builds it.
///
/// Deliberately the client's sequence rather than a convenient one: the decks come
/// before the roads because a bridge-flagged street cuts itself out of the deck it
/// rides, and the fills come out from under the carriageways because otherwise
/// three quarters of every street has a second surface beneath it.
fn tessellate(tile: &TileData, tile_m: f32) -> Result<Vec<SurfaceMesh>> {
    let mut meshes = vector_tiles::tessellate_surfaces(tile, tile_m, None)
        .map_err(anyhow::Error::msg)
        .context("ground fills")?;
    let mut decks = vector_tiles::tessellate_bridges(tile, tile_m)
        .map_err(anyhow::Error::msg)
        .context("bridge decks")?;
    let roads = vector_tiles::tessellate_roads(tile, tile_m, &mut decks, None, false)
        .map_err(anyhow::Error::msg)
        .context("road ribbons")?;
    cartopolis_geo::road_stitch::cut_under_roads(&mut meshes, &roads.ground);
    meshes.extend(roads.ground);

    // The roads that run under the ground, and the holes their open approaches
    // need cut in it — without which an entrance is roofed by the ground sheet.
    let tunnels = cartopolis_geo::tunnels::build_tunnels(tile, tile_m, None);
    if !tunnels.openings.is_empty() {
        cartopolis_geo::tunnels::cut_ground(&mut meshes, &tunnels.openings);
    }
    meshes.extend(tunnels.meshes);
    Ok(meshes)
}

/// The tone this draws a surface in.
///
/// **Mostly the map's own palette, and deliberately not for the road sheet.**
/// `SurfaceGroup::base_color` answers pure white for roads, paving, crops and
/// tunnel carriageways, because in the client those carry their surface in a
/// texture and a tint would fight it. This draws no textures, so taking that
/// answer literally paints every road white on pale ground — which is how a city's
/// whole street network came to be invisible in the first shots aimed at it.
fn harness_tone(group: SurfaceGroup) -> Color {
    match group {
        SurfaceGroup::Road(RoadClass::Motorway | RoadClass::Carriageway) => {
            Color::srgb(0.26, 0.26, 0.28)
        }
        SurfaceGroup::Road(RoadClass::Major) => Color::srgb(0.34, 0.33, 0.34),
        SurfaceGroup::Road(RoadClass::Minor) => Color::srgb(0.42, 0.41, 0.41),
        SurfaceGroup::Road(RoadClass::Path) => Color::srgb(0.58, 0.52, 0.44),
        SurfaceGroup::Road(RoadClass::Rail) => Color::srgb(0.40, 0.36, 0.34),
        SurfaceGroup::Paving => Color::srgb(0.70, 0.69, 0.67),
        SurfaceGroup::Crop => Color::srgb(0.76, 0.70, 0.48),
        // A tunnel in colours nothing on the surface uses, and nothing the
        // underlay uses either: a diagnostic colour has to differ from every other
        // diagnostic colour, which took two goes to get right.
        SurfaceGroup::TunnelRoad(_) => Color::srgb(0.95, 0.55, 0.10),
        SurfaceGroup::TunnelStructure => Color::srgb(0.85, 0.72, 0.55),
        other => {
            let [r, g, b] = other.base_color();
            Color::srgb(r, g, b)
        }
    }
}

fn spawn_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    loaded: Res<Loaded>,
    cli: Res<Cli>,
    mut drawn: ResMut<Drawn>,
) {
    for cell in &loaded.cells {
        // Tile x runs east; tile y runs south, which is +z here.
        #[allow(clippy::cast_precision_loss)]
        let cell_origin = Vec3::new(
            (cell.x - loaded.origin.0) as f32 * cell.tile_m,
            0.0,
            (cell.y - loaded.origin.1) as f32 * cell.tile_m,
        );
        for m in &cell.meshes {
            if let Some(want) = &cli.only {
                if !format!("{:?}", m.group)
                    .to_lowercase()
                    .contains(&want.to_lowercase())
                {
                    continue;
                }
            }
            let Some(mesh) = to_bevy_mesh(m) else {
                continue;
            };
            drawn.meshes += 1;
            drawn.triangles += m.indices.len() / 3;
            let base = if cli.plain {
                Color::srgb(0.72, 0.72, 0.70)
            } else {
                harness_tone(m.group)
            };
            commands.spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: base,
                    perceptual_roughness: 0.95,
                    cull_mode: if cli.double_sided {
                        None
                    } else {
                        Some(bevy::render::render_resource::Face::Back)
                    },
                    double_sided: cli.double_sided,
                    ..default()
                })),
                // The depth ladder from `cartopolis_geo::ground_stack`, the same
                // list the client reads. It is a statement about the *data* —
                // which surface belongs over which — so ignoring it here would
                // report an ordering fault of this binary's own making.
                Transform::from_translation(cell_origin + Vec3::Y * m.group.layer().y_offset()),
            ));
        }
    }
    tracing::info!(
        meshes = drawn.meshes,
        triangles = drawn.triangles,
        "tessellated"
    );
}

fn to_bevy_mesh(m: &SurfaceMesh) -> Option<Mesh> {
    if m.positions.is_empty() || m.indices.is_empty() {
        return None;
    }
    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, m.positions.clone());
    let normals = if m.normals.len() == m.positions.len() {
        m.normals.clone()
    } else {
        vec![[0.0, 1.0, 0.0]; m.positions.len()]
    };
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    if m.uvs.len() == m.positions.len() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, m.uvs.clone());
    }
    mesh.insert_indices(bevy::render::mesh::Indices::U32(m.indices.clone()));
    Some(mesh)
}

fn spawn_camera(
    mut commands: Commands,
    loaded: Res<Loaded>,
    cli: Res<Cli>,
    mut images: ResMut<Assets<Image>>,
) {
    let tile_m = loaded.cells.first().map_or(1465.0, |c| c.tile_m);
    let span = loaded.extent();
    let distance = cli.distance.unwrap_or(span * 0.9).max(1.0);
    let (pitch, yaw) = (cli.look.0.to_radians(), cli.look.1.to_radians());
    let dir = Vec3::new(
        yaw.sin() * pitch.cos(),
        pitch.sin(),
        -yaw.cos() * pitch.cos(),
    );
    let eye = match cli.at {
        Some((lat, lng, alt)) => {
            let z = loaded.cells.first().map_or(14, |c| c.z);
            let (fx, fy) = cartopolis_core::world::lat_lng_to_tile_frac(lat, lng, z);
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            Vec3::new(
                ((fx - loaded.origin.0 as f64) * f64::from(tile_m)) as f32,
                alt,
                ((fy - loaded.origin.1 as f64) * f64::from(tile_m)) as f32,
            )
        }
        None => -dir * distance,
    };

    let mut camera = commands.spawn((
        Camera3d::default(),
        Transform::from_translation(eye).looking_at(eye + dir * distance.max(100.0), Vec3::Y),
        Projection::Perspective(PerspectiveProjection {
            far: span * 8.0 + 10_000.0,
            near: (distance * 0.001).max(0.1),
            ..default()
        }),
        // Physically paired with the sun below. Bevy's default is an indoor
        // exposure, and against real sunlight that is several stops over — which
        // flattens every surface to white and takes the shading with it.
        bevy::camera::Exposure {
            ev100: cli.exposure,
        },
        AmbientLight {
            color: Color::srgb(0.7, 0.8, 1.0),
            brightness: 12_000.0,
            ..default()
        },
    ));

    if !cli.window {
        let mut image = Image::new_fill(
            Extent3d {
                width: cli.size.0,
                height: cli.size.1,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0, 0, 0, 255],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        // `RENDER_ATTACHMENT` to draw into it, `COPY_SRC` so the capture can read
        // it back; without either, wgpu refuses the whole frame.
        image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_SRC
            | TextureUsages::RENDER_ATTACHMENT;
        let handle = images.add(image);
        camera.insert(RenderTarget::Image(handle.clone().into()));
        commands.insert_resource(ShotTarget(handle));
    }
}

/// A checkered sheet under everything, so a hole in the ground is unmistakable.
///
/// **Greyscale, and neither end near black or white.** Two coloured attempts each
/// collided with a feature — a magenta underlay hid magenta tunnel carriageways,
/// near-black squares hid tunnel roofs. Colour is what the *features* need to be
/// told apart by, so the underlay gives it up and keeps the one property that
/// identifies it: nothing in a map is checkered.
fn spawn_checker(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    loaded: Res<Loaded>,
    cli: Res<Cli>,
) {
    const DARK: [u8; 4] = [132, 132, 134, 255];
    const LIGHT: [u8; 4] = [196, 196, 198, 255];
    let mut texels = Vec::with_capacity(16);
    for (a, b) in [(DARK, LIGHT), (LIGHT, DARK)] {
        texels.extend_from_slice(&a);
        texels.extend_from_slice(&b);
    }
    let mut image = Image::new(
        Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        texels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    // Nearest and repeating: a filtered checkerboard blurs into flat grey at
    // distance, which is the one thing it must never do.
    image.sampler = bevy::image::ImageSampler::Descriptor(bevy::image::ImageSamplerDescriptor {
        address_mode_u: bevy::image::ImageAddressMode::Repeat,
        address_mode_v: bevy::image::ImageAddressMode::Repeat,
        mag_filter: bevy::image::ImageFilterMode::Nearest,
        min_filter: bevy::image::ImageFilterMode::Nearest,
        ..default()
    });

    // Sized from the cells' own extent, not their *count* — four cells in a square
    // are two tiles across, and scaling by the count made the underlay twelve tiles
    // wide and the dominant thing in every frame.
    let span = loaded.extent() * 1.5;
    let mut sheet = Mesh::from(Plane3d::default().mesh().size(span, span));
    if let Some(bevy::render::mesh::VertexAttributeValues::Float32x2(uvs)) =
        sheet.attribute_mut(Mesh::ATTRIBUTE_UV_0)
    {
        for uv in uvs.iter_mut() {
            uv[0] *= span / 50.0;
            uv[1] *= span / 50.0;
        }
    }
    commands.spawn((
        Mesh3d(meshes.add(sheet)),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color_texture: Some(images.add(image)),
            // Unlit on purpose: this is a measuring instrument, and a shaded
            // underlay would change tone with the sun and start looking like
            // ground.
            unlit: true,
            ..default()
        })),
        Transform::from_xyz(0.0, -cli.under, 0.0),
    ));
}

/// One sun, no shadows.
///
/// Off deliberately rather than for speed: a shadow is a judgement about how the
/// world should *look*, and a dark band across a surface is exactly what a ground
/// fault looks like too.
fn spawn_sun(mut commands: Commands) {
    commands.spawn((
        DirectionalLight {
            illuminance: bevy::light::light_consts::lux::RAW_SUNLIGHT,
            shadow_maps_enabled: false,
            ..default()
        },
        // Mid-morning from the south-east: a grazing angle, because a ground fault
        // reads as shading long before it reads as silhouette.
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, -0.6, -0.9, 0.0)),
    ));
}

/// Capture once the scene has been drawn, then leave.
///
/// **Not on the first frame.** The render pipelines are built and the target image
/// allocated during the frames after `Startup`, so a capture fired immediately
/// reads back a texture nothing has drawn into — a black image and an exit status
/// of zero, which is the most misleading result a harness can produce.
fn capture_when_ready(
    mut commands: Commands,
    cli: Res<Cli>,
    target: Option<Res<ShotTarget>>,
    drawn: Res<Drawn>,
    mut frames: Local<u32>,
    mut fired: Local<bool>,
) {
    let Some(path) = cli.shot.clone() else { return };
    *frames += 1;
    if *fired || *frames < 8 {
        return;
    }
    *fired = true;
    let shot = match target.as_ref() {
        Some(t) => Screenshot::image(t.0.clone()),
        None => Screenshot::primary_window(),
    };
    let (meshes, triangles) = (drawn.meshes, drawn.triangles);
    commands.spawn(shot).observe(
        move |captured: On<ScreenshotCaptured>, mut writer: MessageWriter<AppExit>| {
            match captured.image.clone().try_into_dynamic() {
                Ok(image) => match image.save(&path) {
                    Ok(()) => {
                        tracing::info!(path = %path.display(), meshes, triangles, "wrote");
                    }
                    Err(e) => tracing::error!(error = %e, "could not write the image"),
                },
                Err(e) => tracing::error!(error = %e, "could not convert the capture"),
            }
            writer.write(AppExit::Success);
        },
    );
}
