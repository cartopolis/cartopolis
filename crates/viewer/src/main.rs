//! `carto-view` — draw what the pipeline built, and nothing else.
//!
//! The pipeline owns the structure of the world; this is assumed correct. So a
//! wrong picture here is a statement about the pipeline, which is the whole reason
//! it carries none of the client's rendering — see the manifest.
//!
//! It reads the prepared cells `carto-pipeline ground` writes
//! (`cartopolis_geo::ground_file`) and draws them with stock materials. No tile
//! streamer, no height survey, no simulation, no sign-in, no user interface: what
//! it shows is a function of the files on disk and the camera, and of nothing that
//! happened to finish arriving.
//!
//! ```bash
//! carto-view --cells /data/map/ground --shot /tmp/ground.png --look=-35,45
//! ```
//!
//! Headless by default, because the machines this has to run on are servers and
//! containers with no display at all. `--window` asks for one where there is
//! something to put it on.

use anyhow::{Context, Result};
use bevy::asset::RenderAssetUsages;
// `RenderTarget` is its own component in Bevy 0.19 (required by `Camera`), and it
// lives in `bevy::camera` rather than the prelude.
use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use clap::Parser;

use cartopolis_geo::ground_file::{self, GroundFile};

#[derive(Parser, Resource, Clone)]
#[command(
    name = "carto-view",
    about = "Draw prepared map cells, to confirm what the pipeline built."
)]
struct Cli {
    /// Directory of prepared cells, `{z}/{x}/{y}.cgnd` — what `carto-pipeline
    /// ground --out` wrote.
    #[arg(long, value_name = "DIR")]
    cells: std::path::PathBuf,

    /// Write one frame here and exit.
    #[arg(long, value_name = "PATH")]
    shot: Option<std::path::PathBuf>,

    /// Camera pitch and yaw in degrees. Note the `=`: clap reads a leading `-35`
    /// as a flag otherwise.
    #[arg(long, value_name = "PITCH,YAW", default_value = "-35,45", value_parser = parse_look)]
    look: (f32, f32),

    /// Metres back from the middle of what was loaded. The default frames all of
    /// it, which is what you want once and never again.
    #[arg(long, value_name = "METRES")]
    distance: Option<f32>,

    /// Offscreen size, `WxH`.
    #[arg(long, value_name = "WxH", default_value = "1200x900", value_parser = parse_size)]
    size: (u32, u32),

    /// Open a window instead of rendering offscreen. Needs a display.
    #[arg(long)]
    window: bool,

    /// Draw every surface in one tone instead of its own, so the *shape* of the
    /// ground is what you see.
    ///
    /// The fault this was built to look at is two surfaces fighting over one piece
    /// of ground, and in the map's own palette both of them are usually pale —
    /// which is a large part of why it stayed invisible in a day of captures.
    #[arg(long)]
    plain: bool,
}

fn parse_look(s: &str) -> Result<(f32, f32), String> {
    let (p, y) = s.split_once(',').ok_or("expected PITCH,YAW")?;
    Ok((
        p.trim().parse().map_err(|_| "pitch is not a number")?,
        y.trim().parse().map_err(|_| "yaw is not a number")?,
    ))
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

/// Everything loaded off disk, and where it sits.
#[derive(Resource)]
struct Loaded {
    cells: Vec<GroundFile>,
    /// Ground metres across one cell. A tile's width is a function of latitude and
    /// the file carries only its tile coordinate — which *is* a latitude.
    tile_m: f64,
    /// The cell everything is placed relative to, so vertex positions stay near the
    /// origin: a tile index is five digits and a cell is ~1.5 km, so absolute tile
    /// metres would put a city hundreds of kilometres out and spend the whole of
    /// `f32`'s precision getting there.
    origin: (u32, u32),
}

/// What was drawn, for a run whose picture nobody is watching.
#[derive(Resource, Default)]
struct Drawn {
    meshes: usize,
    triangles: usize,
}

#[derive(Resource)]
struct ShotTarget(Handle<Image>);

fn main() -> Result<()> {
    let cli = Cli::parse();
    let loaded = load(&cli.cells).context("loading prepared cells")?;
    anyhow::ensure!(
        !loaded.cells.is_empty(),
        "no prepared cells under {} — has `carto-pipeline ground --out` run?",
        cli.cells.display()
    );

    let mut app = App::new();
    if cli.window {
        app.add_plugins(DefaultPlugins);
    } else {
        // No window is created at all: `WinitPlugin` is never added and the runner
        // drives the frames instead. There is no display on the machines this is
        // for, so this is the ordinary path rather than a fallback.
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
        .add_systems(Startup, (spawn_scene, spawn_camera, spawn_sun))
        .add_systems(Update, capture_when_ready);
    app.run();
    Ok(())
}

/// Every `.cgnd` under `root`.
///
/// A file that cannot be read is reported and skipped rather than stopping the
/// run: the reason for looking is usually that something is wrong, and refusing to
/// start over one damaged cell of two hundred hides the other hundred and
/// ninety-nine.
fn load(root: &std::path::Path) -> Result<Loaded> {
    let mut cells = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "cgnd") {
                match std::fs::read(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|b| ground_file::decode(&b).map_err(|e| e.to_string()))
                {
                    Ok(cell) => cells.push(cell),
                    Err(e) => tracing::warn!(path = %path.display(), error = %e, "skipped"),
                }
            }
        }
    }
    cells.sort_by_key(|c| (c.z, c.x, c.y));

    let z = cells.first().map_or(14, |c| c.z);
    let origin = (
        middle(cells.iter().map(|c| c.x)),
        middle(cells.iter().map(|c| c.y)),
    );
    let lat = cartopolis_core::world::tile_to_lat_lng(0, i64::from(origin.1), z).0;
    let tile_m = cartopolis_core::world::tile_to_meters(z, lat);
    Ok(Loaded {
        cells,
        tile_m,
        origin,
    })
}

fn middle(values: impl Iterator<Item = u32>) -> u32 {
    let mut v: Vec<u32> = values.collect();
    v.sort_unstable();
    v.get(v.len() / 2).copied().unwrap_or(0)
}

/// One Bevy mesh per prepared mesh, placed by its cell.
///
/// The depth ladder is applied from `cartopolis_geo::ground_stack`, the same list
/// the pipeline and the client both read. That is a statement about the *data* —
/// which surface belongs over which — so the harness has to honour it or it would
/// report a fault in ordering that is really a fault in the harness. How it is then
/// drawn is this binary's own business and deliberately nothing like the client's.
fn spawn_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    loaded: Res<Loaded>,
    cli: Res<Cli>,
    mut drawn: ResMut<Drawn>,
) {
    for cell in &loaded.cells {
        // Tile x runs east; tile y runs south, which is +z here. Both are a plain
        // difference from the origin cell.
        #[allow(clippy::cast_possible_truncation)]
        let cell_origin = Vec3::new(
            ((f64::from(cell.x) - f64::from(loaded.origin.0)) * loaded.tile_m) as f32,
            0.0,
            ((f64::from(cell.y) - f64::from(loaded.origin.1)) * loaded.tile_m) as f32,
        );

        for m in &cell.meshes {
            let Some(mesh) = to_bevy_mesh(m) else {
                continue;
            };
            drawn.meshes += 1;
            drawn.triangles += m.indices.len() / 3;
            let base = if cli.plain {
                Color::srgb(0.72, 0.72, 0.70)
            } else {
                let [r, g, b] = m.group.base_color();
                Color::srgb(r, g, b)
            };
            commands.spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: base,
                    perceptual_roughness: 0.95,
                    ..default()
                })),
                Transform::from_translation(cell_origin + Vec3::Y * m.group.layer().y_offset()),
            ));
        }
    }
    tracing::info!(
        meshes = drawn.meshes,
        triangles = drawn.triangles,
        "spawned prepared ground"
    );
}

/// A prepared mesh as Bevy's. Normals are supplied where the file has none, which
/// is exact rather than an approximation: a ground sheet is flat and faces up.
fn to_bevy_mesh(m: &cartopolis_geo::vector_tiles::SurfaceMesh) -> Option<Mesh> {
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

/// Frame what was loaded, from `--look`, at `--distance` or far enough to see all
/// of it.
fn spawn_camera(
    mut commands: Commands,
    loaded: Res<Loaded>,
    cli: Res<Cli>,
    mut images: ResMut<Assets<Image>>,
) {
    #[allow(clippy::cast_precision_loss)]
    let span = {
        let xs: Vec<u32> = loaded.cells.iter().map(|c| c.x).collect();
        let ys: Vec<u32> = loaded.cells.iter().map(|c| c.y).collect();
        let wide = xs.iter().max().unwrap_or(&0) - xs.iter().min().unwrap_or(&0) + 1;
        let tall = ys.iter().max().unwrap_or(&0) - ys.iter().min().unwrap_or(&0) + 1;
        (f64::from(wide.max(tall)) * loaded.tile_m) as f32
    };
    let distance = cli.distance.unwrap_or(span * 0.9).max(1.0);
    let (pitch, yaw) = (cli.look.0.to_radians(), cli.look.1.to_radians());
    let dir = Vec3::new(
        yaw.sin() * pitch.cos(),
        pitch.sin(),
        -yaw.cos() * pitch.cos(),
    );

    let mut camera = commands.spawn((
        Camera3d::default(),
        Transform::from_translation(-dir * distance).looking_at(Vec3::ZERO, Vec3::Y),
        Projection::Perspective(PerspectiveProjection {
            far: span * 8.0 + 10_000.0,
            near: (distance * 0.001).max(0.1),
            ..default()
        }),
        // Per-camera in Bevy 0.19, not a resource. Enough fill that a surface
        // facing away from the sun is still legible — the fault being looked for
        // is often on the shaded side of a bank.
        AmbientLight {
            color: Color::srgb(0.7, 0.8, 1.0),
            brightness: 400.0,
            ..default()
        },
    ));

    if !cli.window {
        // The camera draws into an image and the capture reads that back.
        // `RENDER_ATTACHMENT` is what makes drawing into it legal and `COPY_SRC`
        // what makes the readback legal; without either, wgpu refuses the whole
        // frame rather than the one operation.
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
        image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_SRC
            | TextureUsages::RENDER_ATTACHMENT;
        let handle = images.add(image);
        camera.insert(RenderTarget::Image(handle.clone().into()));
        commands.insert_resource(ShotTarget(handle));
    }
}

/// One sun, no shadows.
///
/// Shadows are off deliberately rather than for speed: a shadow is the client's
/// judgement about how the world should *look*, and a dark band across a surface
/// is exactly what a ground fault also looks like. The harness answers "is this
/// surface where it should be", so it lights the scene plainly and leaves the
/// atmosphere, the bloom, the tone mapping and the shadow cascades to the client.
fn spawn_sun(mut commands: Commands) {
    commands.spawn((
        DirectionalLight {
            illuminance: 12_000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        // Mid-morning from the south-east: a grazing angle, because a ground fault
        // reads as shading long before it reads as silhouette.
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, -0.6, -0.9, 0.0)),
    ));
}

/// Capture once the scene has actually been drawn, then leave.
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
    // A handful of frames, and no settling beyond that: nothing in this viewer
    // arrives late, which is the property that makes it fast and repeatable.
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
