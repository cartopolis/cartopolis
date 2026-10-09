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

use std::io::Read;

mod bridges;

// Bound to the name `ContextExt` on purpose: bevy 0.20's prelude glob-imports its own
// `ContextExt`, whose `.context()` is ambiguous with anyhow's. An explicit import
// shadows a glob one, which takes bevy's trait out of scope here.
use anyhow::{Context as ContextExt, Result};
use bevy::asset::RenderAssetUsages;
// `RenderTarget` is its own component in Bevy 0.19 (required by `Camera`), and it
// lives in `bevy::camera` rather than the prelude.
use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use clap::Parser;

use cartopolis_geo::road_grade;
use cartopolis_geo::road_texture::RoadClass;
use cartopolis_geo::terrain_refine;
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

    /// Capture the same viewpoint twice, five centimetres apart, and report the
    /// pixels that changed. A depth fight between two coplanar surfaces is decided
    /// by rounding and re-decided by any camera move, so it is invisible in a
    /// single still and obvious across a pair — this is the instrument that says
    /// whether a road is fighting the ground, rather than a person squinting.
    ///
    /// **Pair it with `--plain` to measure depth.** Measured over a Groningen
    /// motorway on 2026-09-19: the same viewpoint reports about **100** changed
    /// pixels with the road textures on and **0** with `--plain`. Every one of
    /// those is the sampler — fine markings and rail heads resolving differently
    /// under a sub-pixel nudge — and adding the client's mip chain moved it by
    /// five. Textured, this measures surface *and* sampler; plain, it measures
    /// geometry alone, which is what a coplanar fault lives in.
    #[arg(long)]
    flicker: bool,

    /// Camera pitch and yaw in degrees, **paired with `--at` by position** the way
    /// the client's shot harness pairs them. Note the `=`: clap reads a leading
    /// `-35` as a flag otherwise.
    ///
    /// Fewer `--look` than `--at` is fine — the last one given carries on, so a
    /// sweep down one street needs the angle stated once.
    #[arg(long, value_name = "PITCH,YAW", value_parser = parse_look)]
    look: Vec<(f32, f32)>,

    /// Stand the camera at `lat,lng,altitude`. The same convention
    /// `docs/shots.toml` uses, so a viewpoint found once works in either
    /// instrument.
    ///
    /// **Repeatable, and that is most of what this harness is for.** Starting the
    /// engine costs about three seconds and drawing a viewpoint costs under one,
    /// so eight viewpoints in one launch is a few seconds while eight launches is
    /// most of a minute. Each capture is written beside `--shot` tagged with its
    /// index and altitude (`street_02_45m.png`), so a set of them sorts in the
    /// order it was asked for rather than arriving as anonymous files.
    #[arg(long, value_name = "LAT,LNG,ALT", value_parser = parse_at)]
    at: Vec<(f64, f64, f32)>,

    /// Read `--at`'s altitude as metres **above the ground** rather than above
    /// sea level.
    ///
    /// **What makes a `--relief` A/B comparable at all.** With an absolute
    /// altitude the terrain rises under a fixed camera, so the relief picture and
    /// the flat one are taken from different heights and frame different amounts
    /// of ground — and every difference between them then reads as a change in
    /// the geometry. That cost an hour on 2026-09-19, chasing a hole that was two
    /// cameras looking at two places.
    #[arg(long)]
    agl: bool,

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

    /// Stand the ground on the **measured** Dutch ground (AHN), fetched once for
    /// the loaded cells.
    ///
    /// `--relief` is synthetic and is for reproducing a fault on demand. This is
    /// the real thing, and it is what the client draws: same service, same
    /// coverage, same 0.5 m source raster, resampled to whatever grid the box
    /// needs. Outside the Netherlands there is nothing to fetch and the ground
    /// stays flat, which is what the client does there too.
    ///
    /// One blocking request, no cache and no retry — a harness run asks once and
    /// exits, where the client is streaming under a moving camera.
    #[arg(long)]
    ahn: bool,

    /// Stand the ground on synthetic relief of this amplitude, in metres.
    ///
    /// **This is how the road tearing is reproduced at all.** Every surface is
    /// bent onto the ground against its *own* triangles, so two of them disagree
    /// by the ground's curvature between their corners — which on flat ground is
    /// nothing. Flat, the fault is invisible; on a slope it is the whole problem.
    ///
    /// Synthetic rather than the real survey on purpose: no network, identical
    /// every run, and the amplitude can be turned up until the fault is
    /// unmistakable instead of marginal.
    #[arg(long, value_name = "METRES", default_value_t = 0.0)]
    relief: f32,

    /// Flatten the ground along the roads before anything stands on it
    /// (`cartopolis_geo::road_grade`), so the fix can be seen against `--relief`
    /// alone.
    #[arg(long)]
    grade: bool,

    /// Leave the landuse fills under the carriageways instead of cutting them out.
    ///
    /// **The A/B for the fix that landed on 2026-09-19.** 67 % of the road sheet
    /// had built land drawn beneath it; flat that is invisible, and on a gradient
    /// the lower sheet stands through the upper one.
    #[arg(long)]
    no_cut: bool,

    /// Write every measurement as JSON here — the check numbers, and `flicker_px`
    /// for each viewpoint of a `--flicker` run. A bare object for one viewpoint, an
    /// array for several, matching `--dump-state` in the client.
    #[arg(long, value_name = "PATH")]
    dump_state: Option<std::path::PathBuf>,

    /// Assert a measurement, repeatably: `--expect 'flicker_px<=8'`. Quote it — `>`
    /// is a shell redirect.
    ///
    /// Two deliberate choices, both the client's. An **unknown key is a hard error
    /// at startup**, because an assertion that quietly holds is worse than none: a
    /// green run then claims something nobody checked. And a failure does **not**
    /// abort the run, so the remaining viewpoints are still captured and the
    /// pictures that explain the failure come back with it.
    #[arg(long, value_name = "KEY OP VALUE")]
    expect: Vec<String>,

    /// Write a road class's generated surface straight out as a PNG and exit —
    /// `--texture carriageway`.
    ///
    /// **The direct instrument for the markings.** Judging a lane line off a
    /// capture means finding a road of the right class, framing it near enough
    /// that the texture is not aliasing, and then reading a pattern that
    /// perspective is foreshortening. This is the pixels themselves, one texel
    /// per pixel, against a ruler: `u` across, metres along.
    #[arg(long, value_name = "CLASS")]
    texture: Option<String>,

    /// List every layer in the tile, its feature count and its `kind` values,
    /// then exit. What is actually in the tile, as opposed to what this build
    /// makes of it.
    #[arg(long)]
    census: bool,

    /// Metres to lift a bridge deck and the roads it carries.
    ///
    /// **Not a measurement and cannot be one**: a deck's height is not in the
    /// tile. The client measures it from AHN per deck, some frames after the
    /// geometry lands. A flat lift answers what this harness is for — is the road
    /// continuous, where are the joins, what does the deck cut out of the ground
    /// — and says nothing about how high any real bridge is.
    #[arg(long, value_name = "METRES", default_value_t = 5.0)]
    deck_height: f32,

    /// Stand the ground on atlas's **graded** terrain tiles from this host
    /// (`http://127.0.0.1:4790`) — what the client samples where atlas has graded,
    /// approaches already ramped to each deck's measured top. Replaces `--ahn`.
    #[arg(long, value_name = "URL")]
    atlas: Option<String>,

    /// Read the decks atlas measured and the BGT's piers from this data host (the
    /// tree `3dbag.cartopolis.org` serves), and stand each deck along its measured
    /// profile with its piers under it instead of the flat `--deck-height`. With
    /// `--atlas` it also prints how far each deck's top is from the ground three
    /// metres out along its approaches — the client's `deck_step_*`.
    #[arg(long, value_name = "URL")]
    site: Option<String>,

    /// Print the tile's street centre lines within this many metres of `--at`,
    /// in tile-local metres, and exit.
    ///
    /// **The end of guessing at a shape.** A roundabout drew a tooth across its
    /// own island and five synthetic reconstructions of it all behaved; what was
    /// missing was never a sixth guess but the coordinates the encoder actually
    /// sent. A picture shows that something is wrong with a shape and never what
    /// the shape is.
    #[arg(long, value_name = "METRES")]
    dump_lines: Option<f32>,

    /// Measure what was built and print it, instead of drawing.
    ///
    /// **The half of this harness a picture cannot be.** Both faults chased on
    /// 2026-09-19 were ones a camera answers badly: two surfaces covering one
    /// piece of ground is invisible until the ground has a gradient, and a tunnel
    /// three cells away looks exactly like a tunnel that was never built. Numbers
    /// answer both in the time it takes to read them.
    #[arg(long)]
    check: bool,
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

/// One tunnel `build_tunnels` decided on, as `--check` reports it.
///
/// **The place is the part that was missing.** A count of tunnels and how much of
/// each is roofed says whether the rules fired; it does not say where to stand to
/// look at one, and every viewpoint over a lid in this tree was found by hand from
/// a tile index and arithmetic done twice. `mid` is the midpoint of the centre
/// line on Earth, which is what `--at` and the client's `--shot` both take.
struct Built {
    width_m: f32,
    length_m: f32,
    covered: usize,
    vertices: usize,
    /// Midpoint of the centre line, WGS84.
    mid: (f64, f64),
    /// Compass bearing along the centre line at its midpoint, degrees.
    bearing: f32,
}

/// One cell's tessellated ground, and where it sits.
struct Cell {
    z: u32,
    x: i64,
    y: i64,
    tile_m: f32,
    meshes: Vec<SurfaceMesh>,
    /// Land features the tessellation drew nothing for. Carried because they are
    /// the readiest explanation for ground nothing covers, and nothing else in the
    /// picture can point at them.
    unhandled: Vec<(String, usize)>,
    /// `(bridge decks, road meshes riding on one)`. Reported by `--check` rather
    /// than logged, because `tessellate` runs during `load` — before Bevy installs
    /// a log subscriber — so anything it logs goes nowhere at all.
    bridges: (usize, Vec<String>),
    /// Ground area the tunnel openings take out, in square metres.
    openings_m2: f32,
    /// What `build_tunnels` decided, per tunnel — see `--check`.
    tunnels: Vec<Built>,
    /// The tunnels themselves, kept because the *ground* needs them: over a lid
    /// and beside a wall the height survey is stale and is put back to the land
    /// (`cartopolis_geo::tunnels::Lidded`). Without these the harness drew a park
    /// where the client drew a trench, which is the one thing an instrument may
    /// not do.
    bores: Vec<cartopolis_geo::tunnels::Tunnel>,
    /// The bridge decks, kept for the same reason `bores` are: the *ground* needs
    /// them. A deck is where an approach road stops being ground and becomes
    /// structure, so it is what `grade_along_roads` pins the embankment to, and a
    /// count could not say where.
    decks: Vec<vector_tiles::BridgeDeck>,
    /// The ground stretches that climb onto those decks, abutment first. What
    /// bounds the approach ramp to the roads that actually reach a structure.
    approaches: Vec<vector_tiles::Approach>,
    /// `(index into meshes, deck)` for each deck slab and each carriageway on one.
    deck_tags: Vec<(usize, u32)>,
    /// Street centre lines in tile-local metres — see `--dump-lines`.
    lines: Vec<vector_tiles::StreetLine>,
    /// Every layer, its feature count and its kinds — see `--census`.
    census: Vec<(String, usize, Vec<(String, usize)>)>,
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
    /// The middle of the loaded cells, in world metres.
    ///
    /// The checkered underlay is hung from here rather than from the origin. The
    /// origin is the *first* cell's north-west corner, so a plane centred there
    /// covered only one quadrant of a single-cell load — and a reference that is
    /// absent over half the picture makes "no checkerboard" mean either "ground
    /// here" or "no reference here", which is the one thing a reference may not do.
    fn center(&self) -> Vec3 {
        let tile_m = self.cells.first().map_or(1465.0, |c| c.tile_m);
        let xs = self.cells.iter().map(|c| c.x);
        let ys = self.cells.iter().map(|c| c.y);
        #[allow(clippy::cast_precision_loss)]
        let mid =
            |lo: i64, hi: i64, origin: i64| ((lo + hi + 1) as f32 * 0.5 - origin as f32) * tile_m;
        Vec3::new(
            mid(
                xs.clone().min().unwrap_or(0),
                xs.max().unwrap_or(0),
                self.origin.0,
            ),
            0.0,
            mid(
                ys.clone().min().unwrap_or(0),
                ys.max().unwrap_or(0),
                self.origin.1,
            ),
        )
    }

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
    // Before the tiles are even read: an assertion that cannot hold should say so
    // in the time it takes to mistype it, not after a scene has been built.
    let tour = Tour {
        expectations: parse_expectations(&cli.expect).map_err(anyhow::Error::msg)?,
        ..Tour::default()
    };
    let loaded = load(&cli).context("reading tiles")?;
    anyhow::ensure!(!loaded.cells.is_empty(), "no cells could be read");

    if let Some(want) = cli.texture.as_ref() {
        return dump_texture(want, cli.shot.as_deref());
    }

    if let Some(radius) = cli.dump_lines {
        let Some(&(lat, lng, _)) = cli.at.first() else {
            anyhow::bail!("--dump-lines needs an --at to measure from");
        };
        for cell in &loaded.cells {
            let (fx, fy) = cartopolis_core::world::lat_lng_to_tile_frac(lat, lng, cell.z);
            #[allow(clippy::cast_precision_loss)]
            let here = [
                ((fx - cell.x as f64) * f64::from(cell.tile_m)) as f32,
                ((fy - cell.y as f64) * f64::from(cell.tile_m)) as f32,
            ];
            println!(
                "{}/{}/{}  around [{:.1}, {:.1}]",
                cell.z, cell.x, cell.y, here[0], here[1]
            );
            for line in &cell.lines {
                if !line
                    .points
                    .iter()
                    .any(|p| (p[0] - here[0]).hypot(p[1] - here[1]) <= radius)
                {
                    continue;
                }
                let pts: Vec<String> = line
                    .points
                    .iter()
                    .map(|p| format!("[{:.2},{:.2}]", p[0] - here[0], p[1] - here[1]))
                    .collect();
                println!(
                    "  {} {} closed={} {} pts: {}",
                    line.kind,
                    line.flags,
                    line.closed,
                    line.points.len(),
                    pts.join(" ")
                );
            }
        }
        return Ok(());
    }

    if cli.census {
        for cell in &loaded.cells {
            println!("{}/{}/{}", cell.z, cell.x, cell.y);
            for (layer, total, kinds) in &cell.census {
                let list: Vec<String> = kinds.iter().map(|(k, n)| format!("{k} {n}")).collect();
                println!("  {layer:<20} {total:>6}   {}", list.join(", "));
            }
        }
        return Ok(());
    }

    if cli.check {
        for cell in &loaded.cells {
            report(cell);
        }
        return Ok(());
    }

    let ground = Ground(build_field(&loaded, &cli));
    // What the client knows about the bridges, off the same published data.
    let bridges = Bridges(match &cli.site {
        Some(site) => loaded
            .cells
            .iter()
            .map(|c| bridges::cell_bridges(site, c))
            .collect(),
        None => Vec::new(),
    });
    if let (Some(field), false) = (ground.0.as_ref(), bridges.0.is_empty()) {
        let graded = cli
            .atlas
            .as_deref()
            .map(|a| bridges::GradedTiles::fetch(&loaded, a))
            .unwrap_or_default();
        let (lifted, profiled, steps, max, mean) =
            bridges::report(&loaded, &bridges.0, field, &graded);
        let piers: usize = bridges.0.iter().map(|b| b.piers.len()).sum();
        println!(
            "bridges: {lifted} decks lifted, {profiled} profiled, {piers} surveyed piers; \
             {steps} approaches, step max {max:.2} m, mean {mean:.2} m"
        );
    }

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
                // **Compile the shader pipelines synchronously, and this is not
                // a preference.** With them compiling in the background the
                // capture fires before anything can be drawn into the target and
                // writes an empty sky — while reporting the triangle count it
                // *would* have drawn and exiting zero. Measured 2026-09-19:
                // turning this off saved 0.8 s a run and produced a blank image
                // that claimed 74,149 triangles. A harness that lies is worse than
                // a slow one.
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
        .insert_resource(tour)
        .insert_resource(ground)
        .insert_resource(bridges)
        .insert_resource(ClearColor(Color::srgb(0.53, 0.68, 0.82)))
        .add_systems(
            Startup,
            (spawn_scene, spawn_camera, spawn_sun, spawn_checker),
        )
        .add_systems(Update, run_tour);
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
        match tessellate(&tile, tile_m, (z, x, y), !cli.no_cut, cli.deck_height) {
            Ok((meshes, bridges, tunnels, bores, openings_m2, decks, approaches, deck_tags)) => {
                cells.push(Cell {
                    z,
                    x,
                    y,
                    tile_m,
                    meshes,
                    bridges,
                    tunnels,
                    bores,
                    openings_m2,
                    decks,
                    approaches,
                    deck_tags,
                    unhandled: vector_tiles::unhandled_land_kinds(&tile),
                    lines: vector_tiles::street_lines(&tile, tile_m),
                    census: vector_tiles::layer_census(&tile),
                });
            }
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
fn tessellate(
    tile: &TileData,
    tile_m: f32,
    cell: (u32, i64, i64),
    cut: bool,
    deck_m: f32,
) -> Result<(
    Vec<SurfaceMesh>,
    (usize, Vec<String>),
    Vec<Built>,
    Vec<cartopolis_geo::tunnels::Tunnel>,
    f32,
    Vec<vector_tiles::BridgeDeck>,
    Vec<vector_tiles::Approach>,
    Vec<(usize, u32)>,
)> {
    let mut meshes = vector_tiles::tessellate_surfaces(tile, tile_m, None)
        .map_err(anyhow::Error::msg)
        .context("ground fills")?;
    let mut decks = vector_tiles::tessellate_bridges(tile, tile_m)
        .map_err(anyhow::Error::msg)
        .context("bridge decks")?;
    let roads = vector_tiles::tessellate_roads(tile, tile_m, &mut decks, None, false)
        .map_err(anyhow::Error::msg)
        .context("road ribbons")?;
    let approaches = roads.approaches.clone();
    if cut {
        cartopolis_geo::road_stitch::cut_under_roads(&mut meshes, &roads.ground);
    }
    meshes.extend(roads.ground);

    // **The bridges, which this harness used to throw away.**
    //
    // `tessellate_roads` hands back a deck's carriageways separately, tagged with
    // the deck they belong to and still at `y = 0`, because their height is not in
    // the tile: the client measures it from AHN per deck, some frames later. This
    // read `roads.ground` and dropped `on_deck` on the floor, so **every
    // bridge-carried road was missing from every picture** — and the hole it left
    // is a clean square-ended gap in an otherwise continuous road, which is
    // exactly what a real tessellation fault looks like. I spent a while today
    // measuring one of those over the Groningen railway before noticing it was
    // the instrument.
    //
    // There is no AHN here, so the lift is a flat `--deck-height` rather than a
    // measurement. That is honest for what this harness answers — whether the
    // geometry is continuous, where the joins are, what the deck cuts out of the
    // ground — and it is not evidence about a deck's real height, which nothing
    // in a tile could be.
    // `(mesh index, deck index)` for every mesh that is a deck or rides one, so a
    // measured deck can be stood on its profile (`--site`).
    let mut deck_tags: Vec<(usize, u32)> = Vec::new();
    for (i, deck) in decks.iter().enumerate() {
        if deck.indices.is_empty() {
            continue;
        }
        #[allow(clippy::cast_possible_truncation)]
        deck_tags.push((meshes.len(), i as u32));
        meshes.push(SurfaceMesh {
            group: SurfaceGroup::BridgeDeck,
            positions: deck
                .positions
                .iter()
                .map(|p| [p[0], p[1] + deck_m, p[2]])
                .collect(),
            normals: vec![[0.0, 1.0, 0.0]; deck.positions.len()],
            uvs: Vec::new(),
            colors: Vec::new(),
            rungs: Vec::new(),
            indices: deck.indices.clone(),
        });
    }
    let bridges = (
        decks.len(),
        roads
            .on_deck
            .iter()
            .map(|(_, m)| format!("{:?}", m.group))
            .collect::<Vec<_>>(),
    );
    for (d, mut m) in roads.on_deck {
        for p in &mut m.positions {
            p[1] += deck_m;
        }
        deck_tags.push((meshes.len(), d));
        meshes.push(m);
    }

    // The roads that run under the ground, and the holes their open approaches
    // need cut in it — without which an entrance is roofed by the ground sheet.
    let tunnels = cartopolis_geo::tunnels::build_tunnels(tile, tile_m, None);
    if !tunnels.openings.is_empty() {
        cartopolis_geo::tunnels::cut_ground(&mut meshes, &tunnels.openings);
    }
    // Area the tunnel openings deliberately take out of the ground, so a capture
    // can tell "the tunnel cut this" from "the map has no ground here" — which a
    // picture cannot, and which cost this session hours over a building site that
    // happened to sit on a tunnel.
    let opening_m2: f32 = tunnels
        .openings
        .iter()
        .map(|ring| {
            let mut a = 0.0f32;
            for w in ring.windows(2) {
                a += w[0][0] * w[1][1] - w[1][0] * w[0][1];
            }
            (a * 0.5).abs()
        })
        .sum();
    let built: Vec<Built> = tunnels
        .tunnels
        .iter()
        .map(|t| {
            let len: f32 = t
                .centre
                .windows(2)
                .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
                .sum();
            // The midpoint by *index* rather than by arc length: a centre line
            // here is a handful of vertices and the question is only "where do I
            // stand to look at this", which a vertex answers as well as an exact
            // halfway point and without the walk.
            let mid = t.centre[t.centre.len() / 2];
            let (a, b) = (t.centre[0], t.centre[t.centre.len() - 1]);
            // The **camera's** yaw for looking along this tunnel, not a compass
            // bearing: yaw turns anticlockwise from north here and in the client
            // (see `viewpoint`), so this is printed ready to paste into either
            // program rather than ready to read off a map.
            let bearing = (a[0] - b[0])
                .atan2(a[1] - b[1])
                .to_degrees()
                .rem_euclid(360.0);
            let (z, cx, cy) = cell;
            let (lat, lng) = cartopolis_core::world::tile_frac_to_lat_lng(
                cx as f64 + f64::from(mid[0] / tile_m),
                cy as f64 + f64::from(mid[1] / tile_m),
                z,
            );
            Built {
                width_m: t.half_w * 2.0,
                length_m: len,
                covered: t.covered.iter().filter(|c| **c).count(),
                vertices: t.covered.len(),
                mid: (lat, lng),
                bearing,
            }
        })
        .collect();
    meshes.extend(tunnels.meshes);
    Ok((
        meshes,
        bridges,
        built,
        tunnels.tunnels,
        opening_m2,
        decks,
        approaches,
        deck_tags,
    ))
}

/// Samples per side the coverage and overlap measurements use. 512 over a
/// ~1.5 km cell is ~2.9 m — fine enough to tell a missing block from the sliver a
/// kerb leaves, coarse enough to cost nothing beside the tessellation.
const CHECK_SAMPLES: usize = 1024;

/// What one cell's tessellation produced, and what is wrong with it.
///
/// Printed rather than drawn, because the two faults this harness exists for are
/// both ones a picture answers badly. **Two surfaces covering one piece of ground**
/// is invisible until the ground has a gradient — it was 67 % of the road sheet
/// here and nobody had seen it — and **a tunnel** three cells away is
/// indistinguishable from one that was never built.
fn report(cell: &Cell) {
    println!(
        "{}/{}/{}  {:.0} m across",
        cell.z, cell.x, cell.y, cell.tile_m
    );

    // Per group: how much was built, and how much of the cell it covers.
    struct Surveyed {
        name: String,
        /// A sheet lies *in* the ground plane; anything else stands on it, and the
        /// two cannot be compared for overlap.
        sheet: bool,
        /// Which rung of the depth ladder it draws on — what decides whether an
        /// overlap with another group is settled or unarbitrable.
        layer: cartopolis_geo::ground_stack::GroundLayer,
        triangles: usize,
        mask: Vec<bool>,
    }

    let mut groups: Vec<Surveyed> = Vec::new();
    for m in &cell.meshes {
        let name = format!("{:?}", m.group);
        let slot = match groups.iter().position(|g| g.name == name) {
            Some(i) => i,
            None => {
                groups.push(Surveyed {
                    name,
                    sheet: m.group.layer().is_sheet(),
                    layer: m.group.layer(),
                    triangles: 0,
                    mask: vec![false; CHECK_SAMPLES * CHECK_SAMPLES],
                });
                groups.len() - 1
            }
        };
        groups[slot].triangles += m.indices.len() / 3;
        rasterise(m, cell.tile_m, &mut groups[slot].mask);
    }
    groups.sort_by(|a, b| b.triangles.cmp(&a.triangles));

    let mut any = vec![false; CHECK_SAMPLES * CHECK_SAMPLES];
    for g in &groups {
        for (a, m) in any.iter_mut().zip(&g.mask) {
            *a |= *m;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let pct = |n: usize| 100.0 * n as f32 / (CHECK_SAMPLES * CHECK_SAMPLES) as f32;
    println!("  covered {:.1} % of the cell", pct(count(&any)));
    // **Where the uncovered ground comes from.** Two very different things look
    // identical in a capture: a hole the tunnel cut on purpose, and ground the map
    // carries no polygon for at all.
    #[allow(clippy::cast_precision_loss)]
    {
        let cell_m2 = cell.tile_m * cell.tile_m;
        let bare = (100.0 - pct(count(&any))) / 100.0 * cell_m2;
        let cut = cell.openings_m2.min(bare);
        println!(
            "  bare ground {:.0} m2 = tunnel openings {:.0} m2 + no ground polygon {:.0} m2",
            bare,
            cut,
            bare - cut
        );
    }
    if !cell.tunnels.is_empty() {
        println!("  {} tunnel(s) built:", cell.tunnels.len());
        for t in &cell.tunnels {
            println!(
                "    {:>5.1} m wide, {:>6.1} m long, {}/{} vertices roofed, \
                 --at {:.5},{:.5} --look=-20,{:.0}",
                t.width_m, t.length_m, t.covered, t.vertices, t.mid.0, t.mid.1, t.bearing
            );
        }
    }
    if cell.bridges.0 > 0 || !cell.bridges.1.is_empty() {
        println!(
            "  {} bridge deck(s), riding on them: {}",
            cell.bridges.0,
            if cell.bridges.1.is_empty() {
                "nothing".to_string()
            } else {
                cell.bridges.1.join(", ")
            }
        );
    }

    for g in &groups {
        let standing = if g.sheet { "" } else { "   (stands)" };
        println!(
            "  {:<26} {:>7} triangles  {:>5.1} %{standing}",
            g.name,
            g.triangles,
            pct(count(&g.mask))
        );
    }

    // **Pairs that cover the same ground.** The depth ladder orders what genuinely
    // overlaps, and it can only do that while every sheet agrees where the ground
    // is — which stops being true the moment the ground has a gradient. So an
    // overlap here is a fault waiting for terrain to reveal it.
    let mut printed = false;
    for (i, a) in groups.iter().enumerate() {
        for b in groups.iter().skip(i + 1) {
            // **Only sheets are compared.** A tunnel roof sits directly above its
            // own deck and a tree stands on the ground beneath it; both overlap
            // completely in plan and neither is a fault, because real height
            // separates them. The ladder makes exactly this distinction
            // (`GroundLayer::is_sheet`), so the check uses it rather than
            // inventing a second rule — without it every tunnel reads as a 100 %
            // overlap and the measurement is noise.
            if !(a.sheet && b.sheet) {
                continue;
            }
            let both = a
                .mask
                .iter()
                .zip(&b.mask)
                .filter(|(x, y)| **x && **y)
                .count();
            let own = count(&a.mask).max(1);
            #[allow(clippy::cast_precision_loss)]
            let share = 100.0 * both as f32 / own as f32;
            if share >= 1.0 {
                if !printed {
                    println!("  sharing ground:");
                    printed = true;
                }
                // **What, if anything, can order this pair — and note that being
                // ordered is not the same as being safe.**
                //
                // The ladder separates sheets by *millimetres* (`SHEET_UNIT_M`).
                // Two sheets over one piece of ground disagree about where that
                // ground is by the terrain's curvature between their own
                // vertices, which is decimetres on any gradient. Millimetres
                // lose. So an overlap across two rungs is settled on flat ground
                // and tears on a slope — which is precisely the 78 % of the road
                // sheet that had built land beneath it, and which is what the
                // fills-under-roads cut removes.
                //
                // The one genuinely settled case is inside the road band, where a
                // kerb and its own tarmac are separated by a per-vertex rung
                // (`vector_tiles::road_rung`) rather than by height — a depth
                // bias does not decay with the ground's shape. This harness
                // cannot *draw* that ordering, having none of the client's
                // depth-bias rendering, so the verdict says so rather than
                // letting a capture be read as evidence either way.
                let verdict = if a.layer == b.layer {
                    if a.layer == cartopolis_geo::ground_stack::GroundLayer::Road {
                        "ordered per vertex inside the road band — not drawable here"
                    } else {
                        "SAME RUNG — nothing orders this, even flat"
                    }
                } else {
                    "different rungs — settled flat, tears on a gradient"
                };
                println!(
                    "    {} and {}: {share:.1} % of {}  ({verdict})",
                    a.name, b.name, a.name
                );
            }
        }
    }
    if !printed {
        println!("  no two sheets share ground");
    }

    // **Geometry past the cell's own edge.** A tile carries a buffer, so a feature
    // crossing a boundary is delivered in *both* tiles — and `road_stitch` cuts
    // within one tile only, so neither copy knows about the other. That is ground
    // covered twice at every cell edge, which the depth ladder has to arbitrate and
    // which tears once the ground has a gradient. `the-ground-stack.md` names it as
    // the case the cut cannot reach; this says how big it is.
    let mut spill = 0usize;
    let mut total = 0usize;
    for m in &cell.meshes {
        for t in m.indices.chunks_exact(3) {
            total += 1;
            let out = t.iter().any(|&i| {
                let v = m.positions[i as usize];
                v[0] < 0.0 || v[2] < 0.0 || v[0] > cell.tile_m || v[2] > cell.tile_m
            });
            if out {
                spill += 1;
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let spill_pct = 100.0 * spill as f32 / total.max(1) as f32;
    println!("  {spill} of {total} triangles reach past the cell edge ({spill_pct:.1} %)");

    if cell.unhandled.is_empty() {
        println!("  every land kind in the tile is drawn");
    } else {
        println!("  land kinds nothing draws:");
        for (kind, n) in &cell.unhandled {
            println!("    {kind:<24} {n:>5} features");
        }
    }
}

/// Mark the samples a mesh's triangles cover. Centre sampling and no
/// anti-aliasing: this measures *coverage*, and a half-covered sample counted by
/// both of two neighbours would read as an overlap that is not there.
fn rasterise(m: &SurfaceMesh, tile_m: f32, hit: &mut [bool]) {
    let n = CHECK_SAMPLES;
    #[allow(clippy::cast_precision_loss)]
    let scale = n as f32 / tile_m.max(1.0);
    for t in m.indices.chunks_exact(3) {
        let p: Vec<[f32; 2]> = t
            .iter()
            .map(|&i| {
                let v = m.positions[i as usize];
                [v[0] * scale, v[2] * scale]
            })
            .collect();
        let (a, b, c) = (p[0], p[1], p[2]);
        // Both ends clamped: a tile carries a buffer, so a ribbon can lie entirely
        // past the grid and a bound on the maximum alone walks off the row.
        let clamp = |v: f32| -> usize {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let i = v.max(0.0) as usize;
            i.min(n - 1)
        };
        let (minx, maxx) = (
            clamp(a[0].min(b[0]).min(c[0]).floor()),
            clamp(a[0].max(b[0]).max(c[0]).ceil()),
        );
        let (miny, maxy) = (
            clamp(a[1].min(b[1]).min(c[1]).floor()),
            clamp(a[1].max(b[1]).max(c[1]).ceil()),
        );
        let side = |u: [f32; 2], v: [f32; 2], q: [f32; 2]| {
            (v[0] - u[0]) * (q[1] - u[1]) - (v[1] - u[1]) * (q[0] - u[0])
        };
        for y in miny..=maxy {
            for x in minx..=maxx {
                #[allow(clippy::cast_precision_loss)]
                let q = [x as f32 + 0.5, y as f32 + 0.5];
                let (s0, s1, s2) = (side(a, b, q), side(b, c, q), side(c, a, q));
                if (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0) || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0) {
                    hit[y * n + x] = true;
                }
            }
        }
    }
}

fn count(mask: &[bool]) -> usize {
    mask.iter().filter(|m| **m).count()
}

/// Samples per side of the **synthetic** height field over the loaded area.
///
/// Matches what the client works at — its field is ~3.1 m on a desktop and 12.5 m
/// on a handset, and the tearing is a function of that spacing. A harness sampling
/// far finer would quietly fix the fault it exists to show.
///
/// The *measured* field sizes itself instead (`measured_field`), off
/// [`cartopolis_geo::ahn::SAMPLE_M`]: a synthetic landform is smooth at any
/// spacing, and real ground with a ten-metre trench cut through it is not.
const FIELD_SAMPLES: usize = 512;

/// The most samples per side one measured field may ask AHN for.
///
/// The client's own grid, and for its reason: this is one request and one raster,
/// 2049² is 12 MB on the wire, and AHN's native half metre over the same box would
/// be a hundred times that.
const MAX_MEASURED_SAMPLES: usize = 2049;

/// Metre-scale roughness in the synthetic ground, in metres.
///
/// What a laser survey actually carries once the landform is subtracted: kerbs,
/// ditches, vehicles, the return off a hedge. 0.4 m is the order of the real
/// thing, and it is the only part of this ground a road corridor is wide enough to
/// smooth — the landform waves are hundreds of metres long and a road follows
/// those quite correctly.
const ROUGHNESS_M: f32 = 0.4;

/// The longest ground edge left unsplit before draping, in metres.
///
/// The client's `MAX_GROUND_EDGE_M` — four times its field spacing, off the shared
/// constant rather than a copy of the number, because the client crate this harness
/// deliberately does not depend on is where the other half of the pair lives.
const MAX_GROUND_EDGE_M: f32 = cartopolis_geo::ahn::SAMPLE_M * 4.0;

/// The ground the surfaces stand on, as a resource so the camera can stand on it
/// too. Built once in `main` rather than inside `spawn_scene`, which is what lets
/// `--agl` read the same heights the meshes were bent onto.
#[derive(Resource)]
struct Ground(Option<Field>);

/// Per loaded cell, what the client knows about its bridges (`--site`).
#[derive(Resource)]
struct Bridges(Vec<bridges::CellBridges>);

impl Ground {
    /// The lowest point of the ground, or zero when it is flat. What the
    /// checkered underlay is hung beneath.
    fn floor(&self) -> f32 {
        self.0.as_ref().map_or(0.0, |f| {
            f.samples.iter().copied().fold(f32::INFINITY, f32::min)
        })
    }
}

/// The ground the surfaces stand on.
struct Field {
    samples: Vec<f32>,
    step_m: f32,
    /// Samples per side. Carried rather than read off a constant because the
    /// measured field sizes itself to the ground it covers, and the synthetic one
    /// does not.
    n: usize,
    /// World position of sample `(0, 0)` — the loaded cells' north-west corner
    /// when that is not the origin cell's.
    origin: [f32; 2],
    /// NAP of height 0, for a field built from absolute heights.
    datum: f32,
}

impl terrain_refine::GroundField for Field {
    fn min_ground_edge_m(&self) -> f32 {
        // Half the field's own spacing, the client's rule: below it there is no
        // measurement left to resolve.
        self.step_m * 0.5
    }

    fn at(&self, x: f32, z: f32) -> f32 {
        Field::at(self, x, z)
    }
}

impl Field {
    /// The level of the land around a point — what a structure below the ground
    /// with nothing measured is stood on, and what the ground over a lid is put
    /// back to.
    fn around(&self, x: f32, z: f32) -> f32 {
        // The client's sampler, to the ring and the count: the highest of eight
        // points on a circle. A mean would be dragged down by the trench itself
        // wherever the ring clips it.
        let r = cartopolis_geo::tunnels::DATUM_M;
        let mut hi = Field::at(self, x, z);
        for k in 0..8 {
            #[allow(clippy::cast_precision_loss)]
            let a = std::f32::consts::TAU * k as f32 / 8.0;
            hi = hi.max(Field::at(self, x + r * a.cos(), z + r * a.sin()));
        }
        hi
    }
}

impl Field {
    /// Bilinear height at a world point, `0.0` outside.
    fn at(&self, x: f32, z: f32) -> f32 {
        let n = self.n;
        let (fx, fz) = (
            (x - self.origin[0]) / self.step_m,
            (z - self.origin[1]) / self.step_m,
        );
        #[allow(clippy::cast_precision_loss)]
        if !(0.0..=(n - 1) as f32).contains(&fx) || !(0.0..=(n - 1) as f32).contains(&fz) {
            return 0.0;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (x0, z0) = (fx.floor() as usize, fz.floor() as usize);
        let (x1, z1) = ((x0 + 1).min(n - 1), (z0 + 1).min(n - 1));
        #[allow(clippy::cast_precision_loss)]
        let (tx, tz) = (fx - x0 as f32, fz - z0 as f32);
        let s = |x: usize, z: usize| self.samples[z * n + x];
        let top = s(x0, z0) + (s(x1, z0) - s(x0, z0)) * tx;
        let bottom = s(x0, z1) + (s(x1, z1) - s(x0, z1)) * tx;
        top + (bottom - top) * tz
    }
}

/// Flatten the height survey along the roads, in place.
///
/// **A built road is graded and a survey is not.** AHN reads the kerbs, the
/// parked cars, the hedge and the return off a passing lorry, so a carriageway
/// draped straight onto it rides every one of them — and the ground beside it,
/// which is refined far more coarsely, cuts across the same bumps as a chord.
/// What that looks like is a ragged green ridge spiking up along the edge of
/// every road.
///
/// This used to live inside the synthetic half of `build_field`, below an early
/// return, so `--grade` did nothing at all on measured ground — a flag that
/// silently did nothing, on the one field where the fault it answers is real
/// (2026-09-21).
fn grade_along_roads(samples: &mut [f32], n: usize, step_m: f32, loaded: &Loaded, deck_m: f32) {
    let mut road = vec![false; n * n];
    let mut tris: Vec<[[f32; 2]; 3]> = Vec::new();
    let mut deck_tris: Vec<[[f32; 2]; 3]> = Vec::new();
    for cell in &loaded.cells {
        #[allow(clippy::cast_precision_loss)]
        let off = (
            (cell.x - loaded.origin.0) as f32 * cell.tile_m,
            (cell.y - loaded.origin.1) as f32 * cell.tile_m,
        );
        for m in &cell.meshes {
            if !matches!(m.group, SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)) {
                continue;
            }
            for t in m.indices.chunks_exact(3) {
                let q = |i: u32| {
                    let v = m.positions[i as usize];
                    [off.0 + v[0], off.1 + v[2]]
                };
                tris.push([q(t[0]), q(t[1]), q(t[2])]);
            }
        }
        for deck in &cell.decks {
            for t in deck.indices.chunks_exact(3) {
                let q = |i: u32| {
                    let v = deck.positions[i as usize];
                    [off.0 + v[0], off.1 + v[2]]
                };
                deck_tris.push([q(t[0]), q(t[1]), q(t[2])]);
            }
        }
    }
    road_grade::mark_roads(&mut road, n, (0.0, 0.0), step_m, &tris);
    road_grade::dilate_mask(&mut road, n, road_grade::DEFAULT_MARGIN_SAMPLES);

    // **Where the ground stops and the structure starts.** Unlike the client, this
    // harness pushes a deck's carriageways into the cell's own meshes, so the
    // marking above has just claimed the roads *on* every deck as ground. Clearing
    // the deck footprint out of the corridor is what stops the relaxation grading
    // the land up to meet a road that is not standing on it — and the samples that
    // survive next to the footprint are the abutments, which is the one place the
    // road genuinely is at deck height.
    let mut deck = vec![false; n * n];
    road_grade::mark_roads(&mut deck, n, (0.0, 0.0), step_m, &deck_tris);
    for i in 0..n * n {
        if deck[i] {
            road[i] = false;
        }
    }
    // **The ramp corridor is the approach roads, not every road.** Ramping through
    // the whole mask raised 18 % of this cell's streets for five bridges, because
    // a raster corridor is one connected shape and the gradient travels through
    // everything touching it — including the street passing *under* the viaduct.
    // `tessellate_roads` now hands back the ground stretch of each bridge-flagged
    // street either side of its span, which is the one thing that separates the
    // road climbing onto a structure from the road going under it.
    let mut ramp = vec![false; n * n];
    let mut approach_tris: Vec<[[f32; 2]; 3]> = Vec::new();
    for cell in &loaded.cells {
        #[allow(clippy::cast_precision_loss)]
        let off = (
            (cell.x - loaded.origin.0) as f32 * cell.tile_m,
            (cell.y - loaded.origin.1) as f32 * cell.tile_m,
        );
        for a in &cell.approaches {
            // A centre line has no width, and a sample is only marked when a
            // triangle covers it, so the run is laid out as a quad strip a couple
            // of samples wide. The dilation below is what gives it the corridor;
            // this only has to be thick enough not to fall between samples.
            let half = step_m;
            for w in a.points.windows(2) {
                let (p, q) = (
                    [off.0 + w[0][0], off.1 + w[0][1]],
                    [off.0 + w[1][0], off.1 + w[1][1]],
                );
                let (dx, dz) = (q[0] - p[0], q[1] - p[1]);
                let len = dx.hypot(dz);
                if len < 1e-3 {
                    continue;
                }
                let (nx, nz) = (-dz / len * half, dx / len * half);
                let (a0, a1) = ([p[0] + nx, p[1] + nz], [p[0] - nx, p[1] - nz]);
                let (b0, b1) = ([q[0] + nx, q[1] + nz], [q[0] - nx, q[1] - nz]);
                approach_tris.push([a0, a1, b0]);
                approach_tris.push([a1, b1, b0]);
            }
        }
    }
    road_grade::mark_roads(&mut ramp, n, (0.0, 0.0), step_m, &approach_tris);
    road_grade::dilate_mask(&mut ramp, n, road_grade::DEFAULT_MARGIN_SAMPLES);
    for i in 0..n * n {
        if deck[i] {
            ramp[i] = false;
        }
    }

    let mut pins = vec![f32::NAN; n * n];
    let mut abutments = 0usize;
    for z in 1..n - 1 {
        for x in 1..n - 1 {
            let i = z * n + x;
            if !ramp[i] {
                continue;
            }
            if deck[i - 1] || deck[i + 1] || deck[i - n] || deck[i + n] {
                pins[i] = deck_m;
                abutments += 1;
            }
        }
    }

    let before: Vec<f32> = samples.to_vec();
    let raised = road_grade::ramp_from_pins(
        samples,
        n,
        &ramp,
        &mut pins,
        step_m,
        road_grade::MAX_APPROACH_GRADE,
    );
    let moved = road_grade::flatten_pinned(samples, n, &road, &pins, road_grade::DEFAULT_PASSES);
    // **`println!`, not `tracing`, and the reason is the one `report` gives.**
    // `build_field` runs before the `App` exists, so Bevy has not installed a log
    // subscriber yet and every `tracing` line from here goes nowhere at all. The
    // "graded the ground along the roads" line below has been invisible for its
    // whole life for exactly this reason, which is why the grading looked like it
    // was not running today.
    println!(
        "  approach ramps: {} deck triangles, {abutments} abutment samples, \
         {raised} samples raised",
        deck_tris.len() / 3
    );
    // **How far, not just how many.** A count of moved samples says the pass
    // ran; it does not say whether it changed the ground by half a metre or by
    // a millimetre, and those look identical in a picture.
    let (mut sum, mut worst) = (0.0f64, 0.0f32);
    for (a, b) in samples.iter().zip(&before) {
        let d = (a - b).abs();
        sum += f64::from(d);
        worst = worst.max(d);
    }
    #[allow(clippy::cast_precision_loss)]
    let mean = if moved > 0 { sum / moved as f64 } else { 0.0 };
    println!(
        "  graded along the roads: {} marked, {moved} moved, mean {mean:.3} m, worst {worst:.3} m",
        road.iter().filter(|m| **m).count()
    );
}

/// Build the ground, and optionally grade it along the roads.
///
/// Two crossing waves rather than a dome: a single hill has almost no curvature at
/// the scale a surface's triangles span, and curvature between corners *is* the
/// fault being reproduced.
fn build_field(loaded: &Loaded, cli: &Cli) -> Option<Field> {
    if let Some(atlas) = &cli.atlas {
        return bridges::graded_field(loaded, atlas);
    }
    if cli.ahn {
        let mut field = measured_field(loaded)?;
        // **The measured field is the one that needs this**, and it was the one
        // that never got it: the grading sat below this return, so `--grade` was
        // a flag that silently did nothing on real ground.
        if cli.grade {
            grade_along_roads(
                &mut field.samples,
                field.n,
                field.step_m,
                loaded,
                cli.deck_height,
            );
        }
        return Some(field);
    }
    if cli.relief <= 0.0 {
        return None;
    }
    let n = FIELD_SAMPLES;
    let span = loaded.extent();
    #[allow(clippy::cast_precision_loss)]
    let step_m = span / (n - 1) as f32;

    let mut samples = vec![0.0f32; n * n];
    for z in 0..n {
        for x in 0..n {
            #[allow(clippy::cast_precision_loss)]
            let (wx, wz) = (x as f32 * step_m, z as f32 * step_m);
            // Three scales, and the finest is the one that matters.
            //
            // The first two are landform — the hills a city sits on. The third is
            // **survey roughness**, and without it this ground cannot exercise the
            // grading at all: a laser survey carries metre-scale noise (kerbs,
            // ditches, parked vehicles, the return off a hedge) and that is exactly
            // what makes a draped road follow bumps a built road would never have.
            // With only the long waves the ground is already smooth at the scale of
            // a road corridor, grading changes nothing, and the harness reports a
            // fix as having no effect.
            //
            // Its amplitude is a fraction of the landform's rather than a share of
            // `--relief`: roughness does not grow with the size of the hills.
            let a = (wx / 260.0).sin() * (wz / 310.0).cos();
            let b = (wx / 95.0).sin() * (wz / 120.0).sin() * 0.35;
            let rough = ((wx / 11.0).sin() * (wz / 9.0).cos()
                + (wx / 4.5).cos() * (wz / 5.5).sin() * 0.5)
                * ROUGHNESS_M;
            samples[z * n + x] = cli.relief * (a + b) + rough;
        }
    }

    if cli.grade {
        grade_along_roads(&mut samples, n, step_m, loaded, cli.deck_height);
    }

    let lo = samples.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = samples.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    tracing::info!(span_m = span, step_m, low_m = lo, high_m = hi, "ground");
    Some(Field {
        samples,
        step_m,
        n,
        origin: [0.0, 0.0],
        datum: 0.0,
    })
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
    ground: Res<Ground>,
    mut images: ResMut<Assets<Image>>,
    bridges: Res<Bridges>,
) {
    let field = ground.0.as_ref();
    for (ci, cell) in loaded.cells.iter().enumerate() {
        // Tile x runs east; tile y runs south, which is +z here.
        #[allow(clippy::cast_precision_loss)]
        let cell_origin = Vec3::new(
            (cell.x - loaded.origin.0) as f32 * cell.tile_m,
            0.0,
            (cell.y - loaded.origin.1) as f32 * cell.tile_m,
        );
        let cell_bridges = bridges.0.get(ci);
        for (mi, m) in cell.meshes.iter().enumerate() {
            if let Some(want) = &cli.only {
                if !format!("{:?}", m.group)
                    .to_lowercase()
                    .contains(&want.to_lowercase())
                {
                    continue;
                }
            }
            // A deck the client has a measurement for stands on it, as in the
            // client — not on the ground plus a flat `--deck-height`.
            let measured = cell
                .deck_tags
                .iter()
                .find(|t| t.0 == mi)
                .and_then(|t| bridges::measured(cell_bridges?, t.1));
            let mesh = match (measured, field) {
                (Some(meas), Some(f)) => {
                    let mut stood = m.clone();
                    bridges::stand_on_deck(
                        &mut stood.positions,
                        &mut stood.indices,
                        &mut stood.uvs,
                        meas,
                        f.datum,
                        cli.deck_height,
                    );
                    stood.normals = Vec::new();
                    to_bevy_mesh(&stood, None, cell_origin, &cell.bores)
                }
                _ => to_bevy_mesh(m, field, cell_origin, &cell.bores),
            };
            let Some(mesh) = mesh else {
                continue;
            };
            drawn.meshes += 1;
            drawn.triangles += m.indices.len() / 3;
            // **The real road surface, not a stand-in tone.**
            //
            // `road_texture` is the client's own, in `cartopolis_geo` — lane
            // dashes, centre and edge lines, the kerb band, rail head and
            // sleepers. Without it every road is one flat colour, and a flat
            // colour cannot show a joint: whether two ribbons meet at a junction,
            // whether the markings stay registered round a curve, and whether a
            // slip road's texture runs the way the road does are all *marking*
            // questions before they are geometry questions.
            //
            // `--plain` still overrides it, that being the mode for reading shape
            // rather than surface.
            let texture = if cli.plain {
                None
            } else {
                road_surface_texture(m.group, &mut images)
            };
            let base = if cli.plain {
                Color::srgb(0.72, 0.72, 0.70)
            } else if texture.is_some() {
                // White, so the texture arrives unmodulated — the client's roads
                // are textured for exactly this reason and `base_color` answers
                // white for them.
                Color::WHITE
            } else {
                harness_tone(m.group)
            };
            commands.spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: base,
                    base_color_texture: texture,
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
        // The surveyed piers, from the ground to the deck over each.
        if let (Some(b), Some(f)) = (cell_bridges, field) {
            let (pos, nor, idx, _) = bridges::pier_mesh(cell, b, f, [cell_origin.x, cell_origin.z]);
            if !idx.is_empty() {
                let mut mesh = Mesh::new(
                    bevy::render::mesh::PrimitiveTopology::TriangleList,
                    RenderAssetUsages::RENDER_WORLD,
                );
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
                mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, nor);
                mesh.insert_indices(bevy::render::mesh::Indices::U32(idx));
                drawn.meshes += 1;
                commands.spawn((
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(materials.add(StandardMaterial {
                        base_color: harness_tone(SurfaceGroup::BridgeDeck),
                        perceptual_roughness: 0.95,
                        cull_mode: None,
                        double_sided: true,
                        ..default()
                    })),
                    Transform::from_translation(cell_origin),
                ));
            }
        }
    }
    tracing::info!(
        meshes = drawn.meshes,
        triangles = drawn.triangles,
        "tessellated"
    );
}

fn to_bevy_mesh(
    m: &SurfaceMesh,
    field: Option<&Field>,
    cell_origin: Vec3,
    bores: &[cartopolis_geo::tunnels::Tunnel],
) -> Option<Mesh> {
    if m.positions.is_empty() || m.indices.is_empty() {
        return None;
    }
    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    // **Refine before bending, exactly as the client does.**
    //
    // This comment used to say the opposite — that refining here "would hide the
    // fault this exists to show" — and that was backwards. Earcut answers a land
    // parcel with two triangles, so an unrefined 100 m edge draws a straight chord
    // across whatever the ground does between its ends. The road beside it, which
    // has a vertex every few metres, follows. The two then gap by metres, and the
    // harness drew cracks down every corridor that the client, which refines at
    // ~12.5 m before draping, never draws.
    //
    // Measured 2026-09-19: the gaps scaled linearly with `--relief` — invisible at
    // 2 m, wide open at 25 m — which is what a chord does and what a real fault
    // does not. The refinement is `cartopolis_geo::terrain_refine`, moved out of
    // the client for this, so there is one implementation rather than two that
    // would drift.
    let mut positions = m.positions.clone();
    let mut indices = m.indices.clone();
    let mut uvs = if m.uvs.len() == m.positions.len() {
        m.uvs.clone()
    } else {
        Vec::new()
    };
    let mut normals = Vec::new();
    let mut colors = if m.colors.len() == m.positions.len() {
        m.colors.clone()
    } else {
        Vec::new()
    };
    let mut rungs = Vec::new();
    if let Some(f) = field {
        // **A structure is not ground and must not be treated as it.** Refining
        // its walls against the field splits a vertical quad into a grid following
        // a hillside it does not stand on, and displacing it by the field digs it
        // twice where the survey measured the trench. Built with nothing measured
        // (`build_tunnels`), it is relative to flat land, so it stands on the land
        // around each point — the client's rule where atlas has not been.
        let origin = [cell_origin.x, cell_origin.z];
        if matches!(
            m.group,
            SurfaceGroup::TunnelRoad(_) | SurfaceGroup::TunnelStructure
        ) {
            for p in &mut positions {
                p[1] += f.around(origin[0] + p[0], origin[1] + p[2]);
            }
        } else {
            // The measured ground with the lids back on it — one surface, so the
            // refinement resolves the lift as well as the landform. Applied after
            // refining instead, it tears the ground along every buried road.
            let lidded = cartopolis_geo::tunnels::Lidded {
                ground: f,
                around: &|x, z| f.around(x, z),
                weight: &|p| {
                    let local = [p[0] - origin[0], p[1] - origin[1]];
                    bores.iter().fold(0.0f32, |best, t| {
                        best.max(t.fill_weight(local, cartopolis_geo::tunnels::DATUM_M))
                    })
                },
            };
            terrain_refine::refine_for_terrain(
                &mut positions,
                &mut indices,
                &mut uvs,
                &mut normals,
                &mut colors,
                &mut rungs,
                MAX_GROUND_EDGE_M,
                Some(terrain_refine::Crease {
                    field: &lidded,
                    origin,
                }),
            );
            for p in &mut positions {
                p[1] += terrain_refine::GroundField::at(
                    &lidded,
                    cell_origin.x + p[0],
                    cell_origin.z + p[2],
                );
            }
        }
    }
    let vertex_count = positions.len();
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    if uvs.len() == vertex_count {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    }
    // Indices before the normals: computing them walks the triangles.
    mesh.insert_indices(bevy::render::mesh::Indices::U32(indices));
    // Normals off the geometry actually drawn — a displaced surface lit by flat
    // normals shades as though it were still level. A tunnel keeps the ones it was
    // built with, as it does in the client: its walls are vertical, and re-deriving
    // them from the ground's slope lights them as the hillside they are cut into.
    let volumetric = matches!(
        m.group,
        SurfaceGroup::TunnelRoad(_) | SurfaceGroup::TunnelStructure
    );
    if field.is_some() && !volumetric {
        mesh.compute_normals();
    } else {
        let normals = if m.normals.len() == vertex_count {
            m.normals.clone()
        } else {
            vec![[0.0, 1.0, 0.0]; vertex_count]
        };
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    }
    Some(mesh)
}

fn spawn_camera(
    mut commands: Commands,
    loaded: Res<Loaded>,
    cli: Res<Cli>,
    ground: Res<Ground>,
    mut images: ResMut<Assets<Image>>,
) {
    let span = loaded.extent();
    let (eye, dir) = viewpoint(&cli, &loaded, &ground, 0);
    let distance = cli.distance.unwrap_or(span * 0.9).max(1.0);

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
    ground: Res<Ground>,
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
        // Below the **lowest ground**, not below zero.
        //
        // It used to be a flat plane at `-under`, which is correct only while the
        // ground is flat: `--relief 25` puts the field's floor at about −34 m, so
        // a plane at −30 m stood *through* the ground and showed a checkerboard in
        // the middle of intact surfaces. That is the worst thing a reference can
        // do — it reads exactly like a hole, and on 2026-09-19 it sent this
        // session hunting three separate faults that were all the instrument.
        Transform::from_translation(loaded.center().with_y(ground.floor() - cli.under)),
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

/// Frames to let pass before the shutter.
///
/// **Not one.** The render pipelines are built and the target image allocated
/// during the frames after `Startup`, so a capture fired immediately reads back a
/// texture nothing has drawn into — a black image and an exit status of zero,
/// which is the most misleading result a harness can produce. Four is what it
/// takes here with shaders compiling in the background; raise it if a capture ever
/// comes back empty, because that is what an empty one means.
const FRAMES_BEFORE_CAPTURE: u32 = 4;

/// How many viewpoints a run visits — the number of `--at`, or one when none was
/// given and the camera is framing what was loaded.
fn viewpoint_count(cli: &Cli) -> usize {
    cli.at.len().max(1)
}

/// Where the camera stands for viewpoint `i`, and which way it points.
///
/// `--look` is paired with `--at` **by position**, and the last one given carries
/// on past the end of the list — a sweep down one street states its angle once.
fn viewpoint(cli: &Cli, loaded: &Loaded, ground: &Ground, i: usize) -> (Vec3, Vec3) {
    let (pitch_d, yaw_d) = cli
        .look
        .get(i)
        .or_else(|| cli.look.last())
        .copied()
        .unwrap_or((-35.0, 45.0));
    let (pitch, yaw) = (pitch_d.to_radians(), yaw_d.to_radians());
    // **The client's convention, sign and all.** Its free camera is
    // `Quat::from_euler(YXZ, yaw, pitch, 0)` looking down its own −Z, which works
    // out to `(−cos p · sin y, sin p, −cos p · cos y)`: yaw turns *anticlockwise*
    // from north. This was the mirror image of it, so the same `--at` and `--look`
    // in the two programs framed two different views — the harness answering for a
    // viewpoint nobody had asked about, which is worse than not answering
    // (2026-09-21: `--look=-18,69` looked up the ring road here and across the city
    // centre there).
    let dir = Vec3::new(
        -yaw.sin() * pitch.cos(),
        pitch.sin(),
        -yaw.cos() * pitch.cos(),
    );
    let tile_m = loaded.cells.first().map_or(1465.0, |c| c.tile_m);
    let eye = match cli.at.get(i) {
        Some(&(lat, lng, alt)) => {
            let z = loaded.cells.first().map_or(14, |c| c.z);
            let (fx, fy) = cartopolis_core::world::lat_lng_to_tile_frac(lat, lng, z);
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let (wx, wz) = (
                ((fx - loaded.origin.0 as f64) * f64::from(tile_m)) as f32,
                ((fy - loaded.origin.1 as f64) * f64::from(tile_m)) as f32,
            );
            let base = if cli.agl {
                ground.0.as_ref().map_or(0.0, |f| f.at(wx, wz))
            } else {
                0.0
            };
            Vec3::new(wx, base + alt, wz)
        }
        None => -dir * cli.distance.unwrap_or(loaded.extent() * 0.9).max(1.0),
    };
    (eye, dir)
}

/// Where viewpoint `i`'s capture is written.
///
/// Index- **and** altitude-tagged, the client's rule: a filmstrip then sorts in
/// flight order and each frame says from how high it was taken, instead of a
/// directory of files whose only difference is a number nobody can read back.
fn shot_path(cli: &Cli, i: usize) -> Option<std::path::PathBuf> {
    let base = cli.shot.as_ref()?;
    if viewpoint_count(cli) <= 1 {
        return Some(base.clone());
    }
    let stem = base.file_stem().unwrap_or_default().to_string_lossy();
    let ext = base.extension().unwrap_or_default().to_string_lossy();
    #[allow(clippy::cast_possible_truncation)]
    let alt = cli.at.get(i).map_or(0, |a| a.2 as i64);
    Some(base.with_file_name(format!("{stem}_{i:02}_{alt}m.{ext}")))
}

/// How far the camera is nudged between the two captures of a `--flicker` pair.
///
/// Five centimetres is chosen to be *small* rather than representative: honest
/// geometry moves a fraction of a pixel under it, so anything that changes by a
/// whole pixel changed because the depth comparison it depends on came out the
/// other way, not because the view did.
const FLICKER_NUDGE_M: f32 = 0.05;

/// How far apart two channels must be before a pixel counts as changed.
///
/// Not zero. The tonemapper and the rasteriser both carry a last-bit wobble that a
/// sub-pixel camera move re-rolls. Measured on flat ground here the eroded count
/// is exactly **0**, so this harness has no flicker floor at all — unlike the
/// client's, where traffic and pedestrians animate between the pair and the
/// threshold has to sit above them.
const FLICKER_CHANNEL_EPS: i16 = 6;

/// Frames to let pass after the camera moves before the shutter.
///
/// The camera's transform is read during extract, which runs after `Update`, so a
/// capture fired in the same frame as the move photographs the *previous*
/// viewpoint. That is the worst failure a sweep can have: every picture is of the
/// wrong place and every one of them looks fine.
const FRAMES_AFTER_MOVE: u32 = 2;

/// What one viewpoint measured.
#[derive(Clone, Default)]
struct Measurement {
    index: usize,
    at: Option<(f64, f64, f32)>,
    look: (f32, f32),
    meshes: usize,
    triangles: usize,
    flicker_px: Option<u32>,
}

impl Measurement {
    fn json(&self) -> String {
        let mut f = vec![
            format!("\"index\":{}", self.index),
            format!("\"meshes\":{}", self.meshes),
            format!("\"triangles\":{}", self.triangles),
            format!("\"pitch\":{},\"yaw\":{}", self.look.0, self.look.1),
        ];
        if let Some((lat, lng, alt)) = self.at {
            f.push(format!("\"lat\":{lat},\"lng\":{lng},\"alt\":{alt}"));
        }
        if let Some(px) = self.flicker_px {
            f.push(format!("\"flicker_px\":{px}"));
        }
        format!("{{{}}}", f.join(","))
    }

    /// The keys `--expect` may name. An unknown one is refused at startup rather
    /// than silently holding, so this list and the assertion parser cannot drift.
    fn field(&self, key: &str) -> Option<f64> {
        #[allow(clippy::cast_precision_loss)]
        match key {
            "meshes" => Some(self.meshes as f64),
            "triangles" => Some(self.triangles as f64),
            "flicker_px" => self.flicker_px.map(f64::from),
            _ => None,
        }
    }

    const KEYS: &'static [&'static str] = &["meshes", "triangles", "flicker_px"];
}

/// One `--expect`, parsed.
#[derive(Clone)]
struct Expectation {
    raw: String,
    key: String,
    op: String,
    value: f64,
}

/// Parse the assertions, refusing an unknown key.
///
/// At **startup**, before anything is drawn: an assertion naming a field that does
/// not exist would otherwise pass every run and the green result would claim
/// something nobody checked.
fn parse_expectations(raw: &[String]) -> Result<Vec<Expectation>, String> {
    let mut out = Vec::new();
    for r in raw {
        // Longest operators first, or `>=` parses as `>` with a stray `=`.
        let (key, op, value) = ["<=", ">=", "==", "!=", "<", ">"]
            .iter()
            .find_map(|op| r.split_once(op).map(|(k, v)| (k, *op, v)))
            .ok_or_else(|| format!("`{r}` has no operator — try `flicker_px<=8`"))?;
        let key = key.trim().to_string();
        if !Measurement::KEYS.contains(&key.as_str()) {
            return Err(format!(
                "`{r}` names `{key}`, which is not measured. Known: {}",
                Measurement::KEYS.join(", ")
            ));
        }
        let value = match value.trim() {
            "true" => 1.0,
            "false" => 0.0,
            v => v
                .parse()
                .map_err(|_| format!("`{r}` — `{v}` is not a number"))?,
        };
        out.push(Expectation {
            raw: r.clone(),
            key,
            op: op.to_string(),
            value,
        });
    }
    Ok(out)
}

impl Expectation {
    /// Whether the measurement holds. A key the run did not measure — `flicker_px`
    /// without `--flicker` — fails rather than passing vacuously.
    fn holds(&self, m: &Measurement) -> (bool, Option<f64>) {
        let Some(got) = m.field(&self.key) else {
            return (false, None);
        };
        let ok = match self.op.as_str() {
            "<=" => got <= self.value,
            ">=" => got >= self.value,
            "==" => (got - self.value).abs() < f64::EPSILON,
            "!=" => (got - self.value).abs() >= f64::EPSILON,
            "<" => got < self.value,
            ">" => got > self.value,
            _ => false,
        };
        (ok, Some(got))
    }
}

/// Where the tour has got to.
#[derive(Resource, Default)]
struct Tour {
    index: usize,
    stage: u32,
    frames: u32,
    /// Filled by the screenshot observer, which runs when the readback lands
    /// rather than on a frame we choose — so the stage below waits on the value,
    /// not on a count.
    first: std::sync::Arc<std::sync::Mutex<Option<(u32, u32, Vec<u8>)>>>,
    flicker_px: std::sync::Arc<std::sync::Mutex<Option<u32>>>,
    results: Vec<Measurement>,
    expectations: Vec<Expectation>,
}

/// Walk the viewpoints: place the camera, capture, optionally capture again five
/// centimetres over, record, move on.
///
/// One launch for the whole list. Starting the engine costs about three seconds
/// and each further viewpoint under one, so a sweep is the difference between a
/// few seconds and most of a minute — which is the whole reason this harness
/// exists.
#[allow(clippy::needless_pass_by_value)]
fn run_tour(
    mut commands: Commands,
    cli: Res<Cli>,
    loaded: Res<Loaded>,
    ground: Res<Ground>,
    target: Option<Res<ShotTarget>>,
    drawn: Res<Drawn>,
    mut tour: ResMut<Tour>,
    mut cameras: Query<&mut Transform, With<Camera3d>>,
    mut writer: MessageWriter<AppExit>,
) {
    if cli.shot.is_none() && !cli.flicker {
        return;
    }
    tour.frames += 1;
    let count = viewpoint_count(&cli);
    match tour.stage {
        // Let the pipelines build and the target image allocate. A capture fired
        // before that reads back a texture nothing has drawn into — a black image
        // and an exit status of zero, the most misleading result a harness can give.
        0 if tour.frames >= FRAMES_BEFORE_CAPTURE => {
            tour.stage = 2;
            tour.frames = 0;
        }
        // Place the camera for this viewpoint.
        1 => {
            let (eye, dir) = viewpoint(&cli, &loaded, &ground, tour.index);
            for mut t in &mut cameras {
                *t = Transform::from_translation(eye).looking_at(eye + dir * 1000.0, Vec3::Y);
            }
            tour.stage = 2;
            tour.frames = 0;
        }
        2 if tour.frames >= FRAMES_AFTER_MOVE => {
            let slot = std::sync::Arc::clone(&tour.first);
            let keep = (!cli.flicker)
                .then(|| shot_path(&cli, tour.index))
                .flatten();
            commands.spawn(shot_of(target.as_deref())).observe(
                move |captured: On<ScreenshotCaptured>| {
                    let Ok(img) = captured.image.clone().try_into_dynamic() else {
                        tracing::error!("could not convert the capture");
                        return;
                    };
                    if let Some(p) = keep.as_ref() {
                        if let Err(e) = img.save(p) {
                            tracing::error!(error = %e, "could not write the image");
                        } else {
                            tracing::info!(path = %p.display(), "wrote");
                        }
                    }
                    let rgba = img.to_rgba8();
                    let (w, h) = (rgba.width(), rgba.height());
                    *slot.lock().expect("capture slot") = Some((w, h, rgba.into_raw()));
                },
            );
            tour.stage = 3;
            tour.frames = 0;
        }
        // Wait for the readback. Without `--flicker` that is the whole viewpoint.
        3 if tour.first.lock().expect("capture slot").is_some() => {
            if cli.flicker {
                for mut t in &mut cameras {
                    let right = t.right() * FLICKER_NUDGE_M;
                    t.translation += right;
                }
                tour.stage = 4;
                tour.frames = 0;
            } else {
                tour.first.lock().expect("capture slot").take();
                finish_viewpoint(&mut tour, &cli, &drawn, None);
                advance(&mut tour, count, &cli, &mut writer);
            }
        }
        4 if tour.frames >= FRAMES_AFTER_MOVE => {
            let slot = std::sync::Arc::clone(&tour.first);
            let out = std::sync::Arc::clone(&tour.flicker_px);
            let keep = shot_path(&cli, tour.index);
            commands.spawn(shot_of(target.as_deref())).observe(
                move |captured: On<ScreenshotCaptured>| {
                    let Ok(img) = captured.image.clone().try_into_dynamic() else {
                        tracing::error!("could not convert the second capture");
                        return;
                    };
                    let second = img.to_rgba8();
                    let Some((w, h, first)) = slot.lock().expect("capture slot").take() else {
                        tracing::error!("the first capture never landed");
                        return;
                    };
                    if (w, h) != (second.width(), second.height()) {
                        tracing::error!("the pair came back at two different sizes");
                        return;
                    }
                    if let Some(p) = keep.as_ref() {
                        if let Err(e) = img.save(p) {
                            tracing::error!(error = %e, "could not write the image");
                        } else {
                            tracing::info!(path = %p.display(), "wrote");
                        }
                    }
                    *out.lock().expect("flicker slot") =
                        Some(eroded_difference(&first, second.as_raw(), w, h));
                },
            );
            tour.stage = 5;
            tour.frames = 0;
        }
        5 => {
            let px = *tour.flicker_px.lock().expect("flicker slot");
            if let Some(px) = px {
                tour.flicker_px.lock().expect("flicker slot").take();
                finish_viewpoint(&mut tour, &cli, &drawn, Some(px));
                advance(&mut tour, count, &cli, &mut writer);
            }
        }
        _ => {}
    }
}

fn finish_viewpoint(tour: &mut Tour, cli: &Cli, drawn: &Drawn, flicker_px: Option<u32>) {
    let i = tour.index;
    let m = Measurement {
        index: i,
        at: cli.at.get(i).copied(),
        look: cli
            .look
            .get(i)
            .or_else(|| cli.look.last())
            .copied()
            .unwrap_or((-35.0, 45.0)),
        meshes: drawn.meshes,
        triangles: drawn.triangles,
        flicker_px,
    };
    if let Some(px) = flicker_px {
        #[allow(clippy::cast_precision_loss)]
        let share = px as f32 / (cli.size.0 * cli.size.1) as f32 * 100.0;
        tracing::info!(viewpoint = i, flicker_px = px, share_pct = share, "flicker");
    }
    tour.results.push(m);
}

/// Move to the next viewpoint, or finish: write the dump, judge the assertions,
/// and leave with a status that says whether they held.
fn advance(tour: &mut Tour, count: usize, cli: &Cli, writer: &mut MessageWriter<AppExit>) {
    tour.index += 1;
    if tour.index < count {
        tour.stage = 1;
        tour.frames = 0;
        return;
    }
    if let Some(path) = cli.dump_state.as_ref() {
        let body = if tour.results.len() == 1 {
            tour.results[0].json()
        } else {
            format!(
                "[{}]",
                tour.results
                    .iter()
                    .map(Measurement::json)
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        if let Err(e) = std::fs::write(path, body) {
            tracing::error!(error = %e, "could not write the state dump");
        }
    }
    // A failure does not abort the run — every viewpoint has already been
    // captured, so a red result hands back the pictures that explain it.
    let mut failed = false;
    for e in &tour.expectations {
        for m in &tour.results {
            let (ok, got) = e.holds(m);
            if !ok {
                failed = true;
                match got {
                    Some(g) => tracing::error!(
                        expect = %e.raw, viewpoint = m.index, measured = g, "expect failed"
                    ),
                    None => tracing::error!(
                        expect = %e.raw, viewpoint = m.index,
                        "expect failed — the run did not measure it"
                    ),
                }
            }
        }
    }
    writer.write(if failed {
        AppExit::error()
    } else {
        AppExit::Success
    });
}

fn shot_of(target: Option<&ShotTarget>) -> Screenshot {
    match target {
        Some(t) => Screenshot::image(t.0.clone()),
        None => Screenshot::primary_window(),
    }
}

/// Pixels that differ in both captures *and* whose whole 3×3 neighbourhood does.
///
/// The erosion is what separates a depth coin flip from silhouette crawl. An edge
/// between two surfaces moves by a fraction of a pixel under the nudge and
/// re-resolves one pixel wide, so an un-eroded count is dominated by every
/// silhouette on screen. A patch whose depth comparison flipped changes over its
/// whole area at once and survives the erosion nearly intact.
fn eroded_difference(a: &[u8], b: &[u8], w: u32, h: u32) -> u32 {
    let (w, h) = (w as usize, h as usize);
    let mut raw = vec![false; w * h];
    for (i, cell) in raw.iter_mut().enumerate() {
        *cell = (0..3).any(|c| {
            (i16::from(a[i * 4 + c]) - i16::from(b[i * 4 + c])).abs() > FLICKER_CHANNEL_EPS
        });
    }
    let mut count = 0u32;
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            if (y - 1..=y + 1).all(|yy| (x - 1..=x + 1).all(|xx| raw[yy * w + xx])) {
                count += 1;
            }
        }
    }
    count
}

/// The client's own road surface for a group, as a repeating texture.
///
/// `cartopolis_geo::road_texture` is what the client paints its carriageways
/// with, so this is not a second rendering of the roads — it is the same one,
/// minus the mip chain and the anisotropic sampler the client builds around it.
///
/// Both of those omissions are visible and are the point at which a capture stops
/// being evidence: without mips a road at range aliases into a shimmer, which is
/// what a texture-registration fault also looks like. Judge markings **near**, and
/// read a distant road's markings as noise.
fn road_surface_texture(group: SurfaceGroup, images: &mut Assets<Image>) -> Option<Handle<Image>> {
    let class = match group {
        SurfaceGroup::Road(c) | SurfaceGroup::TunnelRoad(c) => c,
        _ => return None,
    };
    let t = cartopolis_geo::road_texture::road_texture(class);
    // **With the client's mip chain**, from `cartopolis_geo::mip`.
    //
    // Without it a road at any distance aliases into a shimmer, and a shimmer is
    // what a texture-registration fault looks like too. Measured on 2026-09-19
    // over a Groningen motorway: a `--flicker` pair reported **105** changed
    // pixels textured and **0** with `--plain`, so every one of them was the
    // sampler rather than the geometry. An instrument whose own noise floor
    // outweighs the thing it measures is not measuring it.
    let (data, mip_count) = cartopolis_geo::mip::build_mip_chain(t.width, t.height, t.rgba.clone());
    // Built from mip 0 and then handed the whole chain: `Image::new` checks the
    // buffer against *one* level's size, so passing the chain to it trips an
    // assertion rather than producing a mipped texture.
    let mut image = Image::new(
        Extent3d {
            width: t.width,
            height: t.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        t.rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = mip_count;
    image.data = Some(data);
    image.sampler = bevy::image::ImageSampler::Descriptor(bevy::image::ImageSamplerDescriptor {
        address_mode_u: bevy::image::ImageAddressMode::Repeat,
        address_mode_v: bevy::image::ImageAddressMode::Repeat,
        address_mode_w: bevy::image::ImageAddressMode::Repeat,
        // The client asks for 8x anisotropy here for the same reason: a road is
        // the surface you almost always see at a grazing angle, where an
        // isotropic filter has to blur along the road to avoid aliasing across it.
        anisotropy_clamp: 8,
        ..bevy::image::ImageSamplerDescriptor::linear()
    });
    Some(images.add(image))
}

/// Write one road class's generated surface as a PNG, with a metre scale.
///
/// The image is the texture's own texels — 64 across the carriageway, 256 along
/// `texture_length_m` metres of it — scaled up by whole numbers so nothing is
/// resampled and a 15 cm line stays exactly as wide as it was generated.
fn dump_texture(want: &str, path: Option<&std::path::Path>) -> Result<()> {
    let class = match want.to_lowercase().as_str() {
        "motorway" => RoadClass::Motorway,
        "carriageway" => RoadClass::Carriageway,
        "major" => RoadClass::Major,
        "minor" => RoadClass::Minor,
        "path" => RoadClass::Path,
        "rail" => RoadClass::Rail,
        other => anyhow::bail!(
            "unknown class `{other}` — motorway, carriageway, major, minor, path, rail"
        ),
    };
    let t = cartopolis_geo::road_texture::road_texture(class);
    let path = path.unwrap_or_else(|| std::path::Path::new("/tmp/road_texture.png"));

    // Whole-number magnification only: resampling a 15 cm line is how a marking
    // check comes to be measuring the filter rather than the marking.
    const ZOOM: u32 = 6;
    let (w, h) = (t.width * ZOOM, t.height * ZOOM);
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let src = ((y / ZOOM) * t.width + (x / ZOOM)) as usize * 4;
            let dst = (y * w + x) as usize * 4;
            out[dst..dst + 4].copy_from_slice(&t.rgba[src..src + 4]);
        }
    }
    image::RgbaImage::from_raw(w, h, out)
        .context("texture did not fit its own dimensions")?
        .save(path)
        .with_context(|| format!("writing {}", path.display()))?;

    let width_m = cartopolis_geo::road_texture::road_width_m(
        match class {
            RoadClass::Motorway => "motorway",
            RoadClass::Carriageway => "motorway",
            RoadClass::Major => "primary",
            RoadClass::Minor => "residential",
            RoadClass::Path => "footway",
            RoadClass::Rail => "rail",
        },
        cartopolis_geo::road_texture::StreetFlags {
            oneway: class == RoadClass::Carriageway,
            ..Default::default()
        },
    );
    tracing::info!(
        path = %path.display(),
        class = ?class,
        across_m = width_m,
        along_m = class.texture_length_m(),
        "wrote texture"
    );
    Ok(())
}

/// The real ground under the loaded cells, from AHN.
///
/// One WCS request over the whole loaded box, decoded with
/// `cartopolis_geo::ahn` — the client's own URL and its own decoder, so this is
/// not a second reading of the service.
///
/// Heights are **relative to the middle of the box**, which is what the client
/// does too: absolute NAP would drop the whole scene by the local ground height
/// for an identical picture. No-data samples (AHN has none over water) are filled
/// from their neighbours rather than left as holes, because a `NaN` in the field
/// propagates into every vertex that samples it.
fn measured_field(loaded: &Loaded) -> Option<Field> {
    let span = loaded.extent();
    let mid = loaded.center();
    // The box centre in lat/lng, via the cell the origin names.
    let (lat, lng) = {
        let c = loaded.cells.first()?;
        let (flat, flng) = cartopolis_core::world::tile_to_lat_lng(
            loaded.origin.0 + (mid.x / c.tile_m).round() as i64,
            loaded.origin.1 + (mid.z / c.tile_m).round() as i64,
            c.z,
        );
        (flat, flng)
    };
    if !cartopolis_geo::ahn::covers(lat, lng) {
        eprintln!("ahn: {lat:.4},{lng:.4} is outside AHN — flat, as in the client");
        return None;
    }
    let rd = cartopolis_geo::ahn::wgs84_to_rd(lat, lng);
    let half = f64::from(span) * 0.5;
    // **As finely as the client bakes it, not as finely as one constant happened
    // to allow.** A fixed 512 samples is 3.1 m over one cell and 5.7 m over four,
    // and 5.7 m is not a coarser picture of the same ground: it aliased the
    // Groningen ring's trench into a crumpled valley with folds and creases down
    // its sides, none of which the client has (measured 2026-09-21). The grid
    // follows the box so that the spacing does not.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = ((span / cartopolis_geo::ahn::SAMPLE_M).round() as usize + 1)
        .clamp(2, MAX_MEASURED_SAMPLES);
    #[allow(clippy::cast_possible_truncation)]
    let grid = n as u32;
    let url = cartopolis_geo::ahn::coverage_url(
        [rd[0] - half, rd[1] - half],
        [rd[0] + half, rd[1] + half],
        grid,
    );
    // `eprintln!`, not `tracing`: this runs inside `load`, before the Bevy app
    // exists, so every `tracing` call here goes to a subscriber that has not been
    // installed. The first version of this logged its request and its relief and
    // printed neither, which on a subtle landform is indistinguishable from the
    // fetch having silently failed.
    eprintln!("ahn: {url}");
    let bytes = match ureq::get(&url)
        .call()
        .map_err(|e| e.to_string())
        .and_then(|r| {
            let mut v = Vec::new();
            r.into_reader()
                .read_to_end(&mut v)
                .map(|_| v)
                .map_err(|e| e.to_string())
        }) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("ahn: request failed ({e}) — the ground stays flat");
            return None;
        }
    };
    let samples = match cartopolis_geo::ahn::decode_float_tiff(&bytes, grid as usize) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ahn: answer could not be read ({e}) — the ground stays flat");
            return None;
        }
    };

    // **Holes are closed from the ground around them, exactly as the client closes
    // them.** AHN is a bare-earth model: there is *no data* under every building,
    // so a city arrives as a field of pits, and this used to drop each one to the
    // box's mean — a metre-deep crater per house. A mesh bent onto that crumples
    // along every street, which is the harness inventing the fault it was built to
    // find (2026-09-21). `fill_holes` is the client's own, shared for this.
    let mut samples = samples;
    cartopolis_geo::ahn::fill_holes(
        &mut samples,
        n,
        Some(cartopolis_geo::ahn::FILL_REACH_M / (span / (n - 1) as f32)),
    );
    let finite: Vec<f32> = samples.iter().copied().filter(|s| s.is_finite()).collect();
    if finite.is_empty() {
        eprintln!("ahn: no data at all over this box — the ground stays flat");
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let mean = finite.iter().sum::<f32>() / finite.len() as f32;
    let samples: Vec<f32> = samples
        .iter()
        .map(|s| if s.is_finite() { *s - mean } else { 0.0 })
        .collect();
    let (lo, hi) = samples
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), s| (a.min(*s), b.max(*s)));
    #[allow(clippy::cast_precision_loss)]
    let step_m = span / (n - 1) as f32;
    eprintln!(
        "ahn: datum {mean:.2} m, low {lo:.2} m, high {hi:.2} m, relief {:.2} m, \
         sample {step_m:.2} m over {n}²",
        hi - lo
    );
    Some(Field {
        samples,
        step_m,
        n,
        origin: [0.0, 0.0],
        datum: mean,
    })
}
