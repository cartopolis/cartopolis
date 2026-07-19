//! Planetary globe / space tier (ADR-025).
//!
//! A true-scale WGS84 Earth built in the sim anchor's ENU tangent frame
//! (`vibe_core::geo`), so the flat streamed-tile city view sits on the sphere's
//! surface at the anchor and Earth's centre lands at ≈ `(0, −R, 0)` in Bevy-local
//! metres. Everything lives under the world `BigSpace`, so `big_space` rebases
//! it for render precision at any camera distance (ADR-019).
//!
//! Imagery is a **whole-globe quadtree LOD** (`update_globe_tiles`): every
//! visible OSM tile is draped on the ellipsoid as a small sphere-patch mesh,
//! and tiles split into their four children whenever they'd exceed a screen-
//! size threshold — so the entire planet sharpens as you approach, not just a
//! patch. A plain ocean-toned base sphere backstops unloaded tiles and the
//! polar caps beyond Web-Mercator's ±85°.
//!
//! The camera side (orbiting Earth's centre from space, and the continuous
//! surface↔space handoff) lives in `free_camera`; this module owns the globe
//! geometry, its tile streaming, and the Earth-centre / radius the camera reads.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::pbr::{AtmosphereMode, AtmosphereSettings};
use bevy_image::{Image, ImageSampler};
use big_space::prelude::{BigSpace, GridCell};

use crate::resources::OsmTileUrlTemplate;
use crate::systems::map_stream::MapStream;
use crate::systems::tile_loader::load_tile_image;
use vibe_core::geo::TangentFrame;
use vibe_core::world::tile_to_meters;
use vibe_core::TileKey;

/// Latitude/longitude tessellation of the base (ocean backstop) sphere.
const GLOBE_RINGS: usize = 64;
const GLOBE_SEGMENTS: usize = 128;

// ── Whole-globe tile quadtree ────────────────────────────────────────────────

/// Quadtree root zoom (z2 = 16 tiles covers the world instantly from cache).
const QT_MIN_Z: u32 = 2;
/// Finest zoom the quadtree refines to; below the space threshold the flat
/// detail pyramid (up to z17+) takes over, so this only needs to look sharp
/// down to ~50 km altitude.
const QT_MAX_Z: u32 = 12;
/// Split a tile into its children once it would exceed this many pixels on
/// screen (roughly: keep every rendered tile near its native 256 px).
const QT_SPLIT_PX: f32 = 340.0;
/// Back off a tile after this many failed attempts…
const QT_MAX_ATTEMPTS: u32 = 3;
/// …and try it again after this cooldown (rate-limited fetches heal instead of
/// leaving permanent holes).
const QT_RETRY_AFTER: Duration = Duration::from_secs(20);
/// Max new fetches queued per selection pass. Bursts of 100+ requests are what
/// trip the tile server's rate limiting (fetches then fail en masse and whole
/// altitude bands render as backstop); a steady trickle loads everything in a
/// few seconds without triggering it.
const QT_FETCH_BUDGET: usize = 24;
/// Zooms kept loaded for the whole planet at all times (z2+z3 = 80 tiles,
/// disk-cached after first run): the guaranteed "blurry but real" floor, so
/// missing mid-zoom tiles degrade to coarse imagery instead of blue backstop.
const QT_BASE_Z: u32 = 3;
/// Re-run quadtree selection at this interval (fetch harvest runs every frame).
const QT_SELECT_INTERVAL: f32 = 0.25;
/// Upper bound on desired *leaf* tiles (backstop against runaway splits); the
/// ancestor chains add on top, so total entities run ~40% higher.
const QT_MAX_TILES: usize = 320;
/// Per-zoom outward lift (m): finer tiles sit slightly above coarser ones, so
/// parents showing through while children load never z-fight.
const QT_LIFT_PER_Z: f32 = 4.0;
/// Mesh tessellation of one tile patch (segments per axis).
const QT_SEGS: usize = 12;
/// How far below the surface the ocean backstop sphere sits. Tile patches are
/// flat-faceted, so a coarse tile's interior sags inside the true sphere by up
/// to R·(1−cos(span/2/QT_SEGS)) — ~14 km for a z2 tile. A backstop at the
/// surface poked through those interiors (tiles rendered as vertex-diamonds
/// with ocean showing between); sunk below the worst-case sag it only shows
/// where no tile exists at all (poles, not-yet-loaded areas).
const BACKSTOP_DEPTH: f64 = 50_000.0;
/// Quadtree selection/streaming only runs above this altitude (below it the
/// globe is hidden and the flat tiles own the view); loaded tiles are kept.
const QT_MIN_ALT: f64 = 35_000.0;

/// Max globe tiles to build (PNG decode + mesh + texture upload) per frame. A
/// zoom-out can land 100+ fetched tiles at once; building them all in one frame
/// packs one giant command buffer that overran a weak GPU's timeout
/// (`VK_ERROR_DEVICE_LOST`). The rest are deferred to later frames — they stay in
/// `pending` (so they aren't re-fetched) and are pushed back into `results`.
const MAX_GLOBE_TILE_BUILDS_PER_FRAME: usize = 4;

/// Max globe tiles to despawn per frame — the mass-*despawn* half of the
/// device-loss guard (its upload half is `MAX_GLOBE_TILE_BUILDS_PER_FRAME`). A
/// zoom-out, stale reclaim, or re-anchor teardown can retire 100+ tiles at once;
/// freeing that many meshes + textures in one command buffer is the same burst
/// that hangs a weak GPU. Retired entities are queued and freed at this rate.
const MAX_GLOBE_TILE_DESPAWNS_PER_FRAME: usize = 8;

#[derive(Resource, Default)]
pub struct GlobeState {
    /// The base (ocean backstop) sphere entity, once built.
    entity: Option<Entity>,
    /// Earth's centre in Bevy-local metres (what the space camera orbits).
    pub center_local: Vec3,
    /// Geocentric radius at the anchor (m) — the sphere's local radius, used for
    /// the camera's altitude and zoom clamps.
    pub radius: f32,
    /// The (lat, lng) the globe is currently built around (the local origin).
    pub anchor_lat: f64,
    pub anchor_lng: f64,
    /// True once the mesh is placed and `center_local`/`radius` are valid.
    pub ready: bool,
    /// The `MapStream::anchor_generation` the current build is for; a change
    /// triggers a rebuild in the new origin's frame.
    built_gen: u32,
    // ── tile quadtree streaming ─────────────────────────────────────────────
    /// Tiles with a live entity (texture applied).
    loaded: HashMap<TileKey, Entity>,
    /// Tiles queued/being fetched.
    pending: HashSet<TileKey>,
    /// Failed fetch attempts per tile: (attempts, last failure) — backed off
    /// after `QT_MAX_ATTEMPTS`, retried after `QT_RETRY_AFTER`.
    failed: HashMap<TileKey, (u32, Instant)>,
    /// Finished fetches from the workers.
    results: Arc<Mutex<Vec<(TileKey, TileFetch)>>>,
    /// Fetch jobs, tagged with the build generation so workers drop stale ones.
    jobs: Option<Sender<(TileKey, u32)>>,
    gen_shared: Arc<AtomicU32>,
    /// The current desired set, shared with the workers: a queued job whose
    /// tile has since left the view (zoom/pan moved on) is cancelled before it
    /// touches the network.
    wanted_shared: Arc<Mutex<HashSet<TileKey>>>,
    /// Countdown to the next quadtree selection pass.
    select_timer: f32,
    /// Last selection's desired set (kept for the despawn policy between runs).
    desired: HashSet<TileKey>,
    /// Tiles retired from `loaded` and awaiting despawn, freed under
    /// `MAX_GLOBE_TILE_DESPAWNS_PER_FRAME` per frame so a mass teardown (zoom-out,
    /// stale reclaim, re-anchor) never packs one giant command buffer.
    despawn_queue: Vec<Entity>,
}

impl GlobeState {
    /// The camera's **geocentric** altitude (m): distance from the Earth's centre
    /// minus the surface radius. This is the only correct "how high am I" once
    /// the camera can orbit the planet — the local-frame `y` is only altitude
    /// near the anchor and goes *negative* on the far side of the globe, which
    /// made every y-based check (tile freeze, globe visibility, fog, skybox)
    /// misfire in space. Falls back to local `y` until the globe exists.
    pub fn altitude_m(&self, cell: &GridCell, tf: &Transform) -> f64 {
        let edge = crate::systems::free_camera::WORLD_CELL_EDGE as f64;
        let cam = DVec3::new(
            cell.x as f64 * edge + tf.translation.x as f64,
            cell.y as f64 * edge + tf.translation.y as f64,
            cell.z as f64 * edge + tf.translation.z as f64,
        );
        if !self.ready {
            return cam.y;
        }
        (cam - self.center_local.as_dvec3()).length() - self.radius as f64
    }

    /// Bevy-local unit direction of Earth's spin axis (points toward the
    /// geographic north pole), in the current anchor frame. The compass
    /// "north up" reset uses it in space, where north-up means aligning the
    /// pole with the top of the screen. Returns `Y` until the globe exists.
    pub fn north_axis_local(&self) -> Vec3 {
        if !self.ready {
            return Vec3::Y;
        }
        let frame = TangentFrame::new(self.anchor_lat, self.anchor_lng, 0.0);
        // ECEF spin axis (0,0,1) → ENU (dot with the frame basis) → Bevy
        // (E, U, −N), the same mapping `sub_point` inverts.
        Vec3::new(frame.east.z as f32, frame.up.z as f32, -frame.north.z as f32)
            .normalize_or_zero()
    }

    /// Whether the camera is high enough that the globe is shown and the local
    /// ground props are hidden — the shared test for every system that flips
    /// per-tier visibility (single source of truth; two systems disagreeing on
    /// this was a real fight, see `apply_feature_toggles`).
    pub fn in_space(&self, cell: &GridCell, tf: &Transform) -> bool {
        self.ready && self.altitude_m(cell, tf) > GLOBE_SHOW_ALT as f64
    }
}

/// Geographic sub-point (lat, lng) of a Bevy-local direction pointing out from
/// the Earth centre, evaluated in the given anchor's frame. Used by the globe
/// descent to learn *where* the camera is dropping toward so it can re-anchor
/// there (ADR-025). `dir_bevy` should be `(cam - earth_centre).normalize()`.
pub fn sub_point(anchor_lat: f64, anchor_lng: f64, dir_bevy: Vec3) -> (f64, f64) {
    let frame = TangentFrame::new(anchor_lat, anchor_lng, 0.0);
    // Bevy (E, U, −N) → ENU (E, N, U), then rotate ENU → ECEF by the frame basis.
    let enu = DVec3::new(dir_bevy.x as f64, -dir_bevy.z as f64, dir_bevy.y as f64);
    let ecef_dir = frame.east * enu.x + frame.north * enu.y + frame.up * enu.z;
    let (lat, lng, _) =
        vibe_core::ecef_to_geodetic(ecef_dir.normalize_or_zero() * 6_378_137.0);
    (lat, lng)
}

/// Bevy-local metres (x = East, y = Up, z = South) of a geodetic point, in the
/// anchor's ENU frame. ENU is (E, N, U); Bevy is (E, U, −N).
fn geodetic_to_local(frame: &TangentFrame, lat: f64, lng: f64, height: f64) -> Vec3 {
    let enu = frame.geodetic_to_local(lat, lng, height);
    Vec3::new(enu.x as f32, enu.z as f32, -enu.y as f32)
}

/// Latitude (deg) of a fractional Web-Mercator tile row at a zoom.
fn tile_frac_to_lat(fy: f64, zoom: u32) -> f64 {
    let n = 2.0_f64.powi(zoom as i32);
    ((1.0 - 2.0 * fy / n) * std::f64::consts::PI).sinh().atan().to_degrees()
}

/// Longitude (deg) of a fractional tile column at a zoom.
fn tile_frac_to_lng(fx: f64, zoom: u32) -> f64 {
    let n = 2.0_f64.powi(zoom as i32);
    fx / n * 360.0 - 180.0
}

/// Does tile `a` cover (is an ancestor of, or equal) tile `b`?
fn covers(a: &TileKey, b: &TileKey) -> bool {
    a.z <= b.z && (b.x >> (b.z - a.z)) == a.x && (b.y >> (b.z - a.z)) == a.y
}

/// Outcome of one worker fetch.
enum TileFetch {
    Ok(Vec<u8>),
    Failed,
    /// The tile left the desired set while queued — dropped without a network
    /// call; not a failure.
    Cancelled,
}

/// Once the map anchor exists, build and place the base globe sphere (and start
/// the tile-fetch workers). Re-runs whenever the anchor moves: the geometry is
/// built in the anchor's tangent frame, so a new origin needs a rebuild.
pub fn setup_globe(
    mut commands: Commands,
    mut globe: ResMut<GlobeState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    stream: Res<MapStream>,
    bigspace: Query<Entity, With<BigSpace>>,
    tile_url: Res<OsmTileUrlTemplate>,
) {
    let Some((clat, clng, _)) = stream.geo_anchor() else {
        return;
    };
    let gen = stream.anchor_generation;
    if globe.ready && globe.built_gen == gen {
        return;
    }
    let Ok(root) = bigspace.single() else {
        return;
    };

    let frame = TangentFrame::new(clat, clng, 0.0);
    // Earth centre (ECEF origin) expressed in the anchor's Bevy-local frame.
    let center_enu = frame.world_to_local(DVec3::ZERO);
    let center = Vec3::new(center_enu.x as f32, center_enu.z as f32, -center_enu.y as f32);
    globe.center_local = center;
    globe.radius = center.length();

    // Build the backstop sphere: a lat/lng grid placed through the ENU frame, so
    // it is the real ellipsoid and the anchor sits on its surface at the origin.
    // Plain ocean tone — the quadtree tiles carry all imagery; this only shows
    // through while tiles load and at the polar caps beyond Mercator's ±85°.
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for i in 0..=GLOBE_RINGS {
        let lat = 90.0 - (i as f64 / GLOBE_RINGS as f64) * 180.0;
        for j in 0..=GLOBE_SEGMENTS {
            let lng = -180.0 + (j as f64 / GLOBE_SEGMENTS as f64) * 360.0;
            let p = geodetic_to_local(&frame, lat, lng, -BACKSTOP_DEPTH);
            positions.push([p.x, p.y, p.z]);
            let n = (p - center).normalize_or_zero();
            normals.push([n.x, n.y, n.z]);
        }
    }
    let stride = (GLOBE_SEGMENTS + 1) as u32;
    for i in 0..GLOBE_RINGS as u32 {
        for j in 0..GLOBE_SEGMENTS as u32 {
            let a = i * stride + j;
            let b = a + stride;
            indices.extend_from_slice(&[a, a + 1, b, b, a + 1, b + 1]);
        }
    }
    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_indices(bevy::render::mesh::Indices::U32(indices));

    // Tear down the previous build (base sphere + every streamed tile): all of
    // it was placed in the old origin's frame.
    if let Some(old) = globe.entity.take() {
        commands.entity(old).despawn();
    }
    // Queue the old anchor's tiles for bounded despawn rather than freeing the
    // whole set in one frame — the mass-despawn burst that hangs a weak GPU.
    // (Below GLOBE_SHOW_ALT they're hidden anyway, so draining over ~1 s is
    // invisible; update_globe_tiles drains the queue every frame.)
    let old_tiles = globe.loaded.drain().map(|(_, e)| e).collect::<Vec<_>>();
    globe.despawn_queue.extend(old_tiles);
    globe.pending.clear();
    globe.failed.clear();
    globe.desired.clear();
    globe.select_timer = 0.0;

    let entity = commands
        .spawn((
            Mesh3d(meshes.add(mesh)),
            // Lit (matte), in the OSM basemap's water tone. The globe must be lit,
            // not unlit: the camera runs a physical `Exposure` (ev100 ≈ 13, for the
            // 130 klux sun), under which an unlit 0–1 base colour tonemaps to
            // near-black — that was the whole globe rendering black from orbit.
            // As a normal PBR surface it exposes like the map tiles: the sunlit day
            // side is bright and the night side is softly filled by the ambient
            // light (not a void). The water tone backstops the polar caps beyond
            // Mercator's ±85° and still-loading tiles.
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgb(0.67, 0.80, 0.85),
                perceptual_roughness: 1.0,
                reflectance: 0.0,
                double_sided: true,
                cull_mode: None,
                ..default()
            })),
            Transform::default(),
            GridCell::default(),
            ChildOf(root),
            // One huge mesh straddling the origin; big_space rebasing confuses
            // frustum culling for it, as with the map tiles.
            NoFrustumCulling,
            bevy::light::NotShadowCaster,
            bevy::light::NotShadowReceiver,
            Visibility::Hidden, // shown by update_globe_visibility only in space
            GlobeMesh,
        ))
        .id();
    globe.entity = Some(entity);
    globe.anchor_lat = clat;
    globe.anchor_lng = clng;
    globe.built_gen = gen;
    globe.gen_shared.store(gen, Ordering::Relaxed);
    globe.ready = true;

    // Fetch workers (once): generation-tagged jobs, so a re-anchor's queued
    // fetches are dropped instead of blocking the fresh build's tiles.
    if globe.jobs.is_none() {
        let (tx, rx) = std::sync::mpsc::channel::<(TileKey, u32)>();
        globe.jobs = Some(tx);
        let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..3 {
            let rx = rx.clone();
            let template = template.clone();
            let results = globe.results.clone();
            let live_gen = globe.gen_shared.clone();
            let wanted = globe.wanted_shared.clone();
            std::thread::spawn(move || loop {
                let Ok((key, job_gen)) = ({
                    let Ok(guard) = rx.lock() else { break };
                    guard.recv()
                }) else {
                    break;
                };
                if job_gen != live_gen.load(Ordering::Relaxed) {
                    continue;
                }
                // Cancel jobs whose tile has since left the view — a zoom
                // sweep queues intermediate levels that are stale before the
                // worker reaches them; don't spend network on those.
                let still_wanted = wanted
                    .lock()
                    .map(|w| w.contains(&key))
                    .unwrap_or(true);
                let outcome = if !still_wanted {
                    TileFetch::Cancelled
                } else {
                    match load_tile_image(&key, &template) {
                        Ok(bytes) => TileFetch::Ok(bytes),
                        Err(_) => TileFetch::Failed,
                    }
                };
                if let Ok(mut g) = results.lock() {
                    g.push((key, outcome));
                }
            });
        }
    }

    tracing::info!(radius = globe.radius, "globe placed");
}

/// Marker for globe imagery entities (base sphere + quadtree tiles).
#[derive(Component)]
pub struct GlobeMesh;

/// Local-scene ground props (the horizon disc, region quads, streamed map tiles)
/// that should be hidden once the globe is in view — from space they read as
/// bright specks/discs floating over the anchor. Attached where each is spawned.
#[derive(Component)]
pub struct HideInSpace;

/// Altitude (m) above which the globe is drawn. Below it the detailed flat-tile
/// city view fully covers the sky-facing hemisphere, so the globe stays hidden
/// (and can't z-fight the tiles that sit on its surface).
pub const GLOBE_SHOW_ALT: f32 = 50_000.0;

/// Marker for the planet-centred entity that carries the built-in atmosphere's
/// [`Atmosphere`] component (spawned in `free_camera::setup_camera`), keeping a
/// copy of that component. `update_atmosphere_origin` reads it to keep the
/// atmosphere centred on Earth in render coords.
///
/// NOTE: do NOT add/remove the `Atmosphere` component (or the camera's
/// `AtmosphereEnvironmentMapLight`) at runtime as an on/off switch — that
/// teardown frees GPU resources still in flight and hangs the device on this
/// stack (confirmed via kernel `GPU HANG` logs). Both are chosen once at startup
/// (`VIBERFOX_NO_ATMOSPHERE` / `VIBERFOX_NO_ENVMAP`); the only safe runtime
/// change is the in-place render-*mode* switch below.
#[derive(Component)]
pub struct PlanetAtmosphere;

/// Render the atmosphere from orbit? `true`: the space tier switches the camera
/// to `AtmosphereMode::Raymarched` (at [`GLOBE_SKY_SAMPLES`]) so the planet shows
/// a real atmospheric limb; `update_atmosphere_origin` keeps the atmosphere's
/// planet centre at the camera's true geocentric offset, which is what makes
/// Raymarched-from-space geometrically possible at all. `false`: stay in
/// `LookupTexture` in space too (no raymarch cost; the globe just loses its
/// atmospheric limb). Either way the components stay put — only the mode changes.
///
/// Forced `false`: on the Intel iGPU the full-screen Raymarched integration from
/// orbit pins every frame near the command-buffer timeout (measured `frame_ms`
/// ~1024 starting the instant altitude crosses `GLOBE_SHOW_ALT`, regardless of
/// tile count), and a streaming hitch then tips it over into
/// `VK_ERROR_DEVICE_LOST`. Even at [`GLOBE_SKY_SAMPLES`] = 6 it's too heavy here.
/// `LookupTexture` in space is a fixed cheap cost and needs no component teardown.
/// Re-enable only behind a GPU-capability check, never unconditionally.
const SPACE_ATMOSPHERE: bool = false;
/// Raymarch sky samples per fragment in the space tier (Bevy's default is 16).
/// The integration runs full-screen from orbit, so keep it lean for weak GPUs;
/// the atmosphere is a thin shell from out there and needs few samples.
const GLOBE_SKY_SAMPLES: u32 = 6;
/// Bevy's default sky samples, restored at ground level (LookupTexture mode
/// only raymarches in its LUT builds, so this is mostly for correctness).
const GROUND_SKY_SAMPLES: u32 = 16;

/// Keep the planet-centred `Atmosphere` entity at Earth's **true centre in
/// render coordinates**, every frame once the globe exists.
///
/// The camera is the big_space `FloatingOrigin`: its `GlobalTransform` stays
/// near the origin at ANY real altitude (big_space rebases the world around
/// it). The atmosphere entity used to be parked at a fixed `(0, −R, 0)`, so
/// bevy_pbr always measured a ground-level camera radius — correct for the flat
/// city view, but in the space tier "Raymarched from orbit" was integrating a
/// *surface* sky dome over the whole screen: visually wrong (washed/black
/// globe) and needlessly expensive, which got misread as "Raymarched can't do
/// space on this GPU". The entity lives OUTSIDE the BigSpace (its Transform is
/// plain render coords), so placing it at `center_local − camera_cell_origin`
/// gives bevy_pbr the exact camera→Earth-centre vector in every tier: at
/// ground level this reproduces the old `(0, −R, 0)`, from orbit the radius
/// grows with the real geocentric altitude and the raymarched limb is correct.
pub fn update_atmosphere_origin(
    globe: Res<GlobeState>,
    camera: Query<&GridCell, With<crate::systems::free_camera::FreeCamera>>,
    mut planet: Query<&mut Transform, With<PlanetAtmosphere>>,
) {
    if !globe.ready {
        return;
    }
    let (Ok(cell), Ok(mut tf)) = (camera.single(), planet.single_mut()) else {
        return;
    };
    let edge = crate::systems::free_camera::WORLD_CELL_EDGE as f64;
    let centre = globe.center_local.as_dvec3()
        - DVec3::new(cell.x as f64, cell.y as f64, cell.z as f64) * edge;
    let new = centre.as_vec3();
    if tf.translation != new {
        tf.translation = new;
    }
}

/// Show the globe only once high enough that it's the thing you're looking at, so
/// normal ground play never pays for it or sees it poke through the tiles. At the
/// same altitude the atmosphere is disabled (see below) so the planet renders lit
/// against black space instead of washing out or hanging the GPU.
#[allow(clippy::type_complexity)]
pub fn update_globe_visibility(
    globe: Res<GlobeState>,
    mut camera: Query<
        (&GridCell, &Transform, &mut AtmosphereSettings),
        With<crate::systems::free_camera::FreeCamera>,
    >,
    mut globe_vis: Query<
        &mut Visibility,
        (With<GlobeMesh>, Without<HideInSpace>),
    >,
    // Only the ground props NOT owned by the Layers toggles (i.e. the horizon
    // disc): tiles, region quads, buildings and windows are excluded because
    // `data_layers::apply_feature_toggles` is their single visibility writer —
    // it combines the panel toggle with `GlobeState::in_space`. Two systems
    // writing the same entities raced nondeterministically (tiles sometimes
    // stayed visible from orbit).
    mut hide_vis: Query<
        &mut Visibility,
        (
            With<HideInSpace>,
            Without<GlobeMesh>,
            Without<crate::systems::map_stream::StreamTile>,
            Without<crate::systems::rendering::RegionMesh>,
            Without<crate::systems::osm_buildings::OsmBuildingMesh>,
            Without<crate::systems::osm_buildings::WindowGlow>,
        ),
    >,
) {
    if !globe.ready {
        return;
    }
    let Ok((cell, tf, mut atmosphere)) = camera.single_mut() else {
        return;
    };
    let show_globe = globe.in_space(cell, tf);

    // Tiered atmosphere *render mode* — the ONLY atmosphere change that's safe at
    // runtime. Near the ground the cheap `LookupTexture` mode suits the city view;
    // from orbit only `Raymarched` renders a planet seen from outside the
    // atmosphere, so switch modes at the globe threshold with a reduced sample
    // count for weak GPUs. Correct only because `update_atmosphere_origin` feeds
    // bevy_pbr the true geocentric camera position (without that, Raymarched from
    // orbit integrated a surface sky dome over the whole screen).
    //
    // We deliberately do NOT add/remove the `Atmosphere` or the camera's
    // `AtmosphereEnvironmentMapLight` here (an earlier version dropped the env-map
    // light in space): tearing those components down at runtime frees GPU
    // resources still in flight and hangs the device (`VK_ERROR_DEVICE_LOST`,
    // confirmed via kernel `GPU HANG` logs). Both are chosen once at startup
    // (`setup_camera`, gated by the dev-panel/env flags) and left in place.
    let (want_mode, want_samples) = if show_globe && SPACE_ATMOSPHERE {
        (AtmosphereMode::Raymarched, GLOBE_SKY_SAMPLES)
    } else {
        (AtmosphereMode::LookupTexture, GROUND_SKY_SAMPLES)
    };
    if atmosphere.rendering_method as u32 != want_mode as u32 {
        atmosphere.rendering_method = want_mode;
    }
    if atmosphere.sky_max_samples != want_samples {
        atmosphere.sky_max_samples = want_samples;
    }

    let want_globe = if show_globe { Visibility::Visible } else { Visibility::Hidden };
    for mut v in globe_vis.iter_mut() {
        if *v != want_globe {
            *v = want_globe;
        }
    }
    // Hide the skybox and the local ground props while the globe is up (so the
    // sky reads as black space and no bright disc floats over the anchor); show
    // them again on the way back down.
    let want_local = if show_globe { Visibility::Hidden } else { Visibility::Inherited };
    for mut v in hide_vis.iter_mut() {
        if *v != want_local {
            *v = want_local;
        }
    }
}

/// Whole-globe quadtree tile streaming: select the tile set whose screen-space
/// size is right for the current view (splitting down to `QT_MAX_Z`), fetch the
/// missing ones (coarse first), and swap tiles in/out without holes — a parent
/// stays until all four children are ready, and vice versa on zoom-out.
#[allow(clippy::too_many_arguments)]
pub fn update_globe_tiles(
    mut commands: Commands,
    mut globe: ResMut<GlobeState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    camera: Query<
        (&GridCell, &Transform, &Projection),
        With<crate::systems::free_camera::FreeCamera>,
    >,
    windows: Query<&Window>,
    bigspace: Query<Entity, With<BigSpace>>,
    time: Res<Time>,
) {
    if !globe.ready {
        return;
    }
    let Ok((cell, tf, projection)) = camera.single() else {
        return;
    };
    let Ok(root) = bigspace.single() else {
        return;
    };
    let frame = TangentFrame::new(globe.anchor_lat, globe.anchor_lng, 0.0);
    let earth_c = globe.center_local.as_dvec3();

    // ── Drain the despawn queue under a per-frame budget (every frame) ──────
    // The mass-despawn half of the device-loss guard, paired with the build cap.
    // Runs above the QT_MIN_ALT early-return below so a ground-level re-anchor
    // teardown still clears while the globe is hidden.
    if !globe.despawn_queue.is_empty() {
        let n = globe.despawn_queue.len().min(MAX_GLOBE_TILE_DESPAWNS_PER_FRAME);
        for e in globe.despawn_queue.drain(..n) {
            commands.entity(e).despawn();
        }
    }

    // ── Harvest finished fetches into live tile entities (every frame) ──────
    let ready: Vec<(TileKey, TileFetch)> = globe
        .results
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    let mut fetch_failures = 0u32;
    let mut built = 0usize;
    let mut deferred: Vec<(TileKey, TileFetch)> = Vec::new();
    for (key, outcome) in ready {
        let bytes = match outcome {
            TileFetch::Cancelled => {
                globe.pending.remove(&key);
                continue; // left the view while queued
            }
            TileFetch::Failed => {
                globe.pending.remove(&key);
                let e = globe.failed.entry(key).or_insert((0, Instant::now()));
                e.0 += 1;
                e.1 = Instant::now();
                fetch_failures += 1;
                continue;
            }
            TileFetch::Ok(bytes) => bytes,
        };
        // Usefulness gate: don't spawn a tile the view has moved past — unless
        // it backstops a desired-but-unloaded descendant. (The bytes are on
        // disk; re-wanting it later is cheap.)
        let useful = globe.desired.contains(&key)
            || globe
                .desired
                .iter()
                .any(|d| covers(&key, d) && !globe.loaded.contains_key(d));
        if !useful {
            globe.pending.remove(&key);
            continue;
        }
        // Cap decode + upload per frame; defer the rest (kept in `pending` so they
        // aren't re-fetched) to keep the tile burst from overrunning the GPU timeout.
        if built >= MAX_GLOBE_TILE_BUILDS_PER_FRAME {
            deferred.push((key, TileFetch::Ok(bytes)));
            continue;
        }
        globe.pending.remove(&key);
        let Ok(img) = image::load_from_memory(&bytes) else {
            let e = globe.failed.entry(key).or_insert((0, Instant::now()));
            e.0 += 1;
            e.1 = Instant::now();
            fetch_failures += 1;
            continue;
        };
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        let mut image = Image::new(
            Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            TextureDimension::D2,
            rgba.into_raw(),
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::default(),
        );
        image.sampler = ImageSampler::linear();
        let texture = images.add(image);

        // Sphere-patch mesh over the tile's Mercator footprint: rows linear in
        // tile-y (so the texture maps 1:1), vertices relative to the tile
        // centre for f32 precision, lifted by zoom so finer tiles sit above.
        let lift = (key.z as f32 * QT_LIFT_PER_Z) as f64;
        let c_lat = tile_frac_to_lat(key.y as f64 + 0.5, key.z);
        let c_lng = tile_frac_to_lng(key.x as f64 + 0.5, key.z);
        let origin = geodetic_to_local(&frame, c_lat, c_lng, lift);
        let mut positions: Vec<[f32; 3]> = Vec::new();
        let mut normals: Vec<[f32; 3]> = Vec::new();
        let mut uvs: Vec<[f32; 2]> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for iy in 0..=QT_SEGS {
            let v = iy as f64 / QT_SEGS as f64;
            let lat = tile_frac_to_lat(key.y as f64 + v, key.z);
            for ix in 0..=QT_SEGS {
                let u = ix as f64 / QT_SEGS as f64;
                let lng = tile_frac_to_lng(key.x as f64 + u, key.z);
                let p = geodetic_to_local(&frame, lat, lng, lift);
                let rel = p - origin;
                positions.push([rel.x, rel.y, rel.z]);
                let n = (p - globe.center_local).normalize_or_zero();
                normals.push([n.x, n.y, n.z]);
                uvs.push([u as f32, v as f32]);
            }
        }
        let stride = (QT_SEGS + 1) as u32;
        for iy in 0..QT_SEGS as u32 {
            for ix in 0..QT_SEGS as u32 {
                let a = iy * stride + ix;
                let b = a + stride;
                // Outward-front winding (x = east, rows march south).
                indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        let mut mesh = Mesh::new(
            bevy::render::mesh::PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
        mesh.insert_indices(bevy::render::mesh::Indices::U32(indices));

        let entity = commands
            .spawn((
                Mesh3d(meshes.add(mesh)),
                // Lit (matte) to match the base sphere: an unlit tile tonemaps to
                // near-black under the camera's physical exposure (see the base
                // sphere note). Sunlit day side bright, night side ambient-filled.
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color_texture: Some(texture),
                    perceptual_roughness: 1.0,
                    reflectance: 0.0,
                    ..default()
                })),
                Transform::from_translation(origin),
                GridCell::default(),
                ChildOf(root),
                NoFrustumCulling,
                bevy::light::NotShadowCaster,
                bevy::light::NotShadowReceiver,
                Visibility::Hidden, // update_globe_visibility shows it in space
                GlobeMesh,
            ))
            .id();
        if let Some(old) = globe.loaded.insert(key, entity) {
            globe.despawn_queue.push(old);
        }
        built += 1;
    }
    // Requeue the tiles we deferred this frame for the next one.
    if !deferred.is_empty() {
        if let Ok(mut g) = globe.results.lock() {
            g.extend(deferred);
        }
    }
    if fetch_failures > 0 {
        // A run of failures usually means the tile server is rate-limiting;
        // failed tiles retry after a cooldown (QT_RETRY_AFTER).
        tracing::warn!(failures = fetch_failures, "globe tile fetches failed");
    }

    // ── Quadtree selection (throttled; skipped near the ground) ─────────────
    let cam = DVec3::new(
        cell.x as f64 * crate::systems::free_camera::WORLD_CELL_EDGE as f64
            + tf.translation.x as f64,
        cell.y as f64 * crate::systems::free_camera::WORLD_CELL_EDGE as f64
            + tf.translation.y as f64,
        cell.z as f64 * crate::systems::free_camera::WORLD_CELL_EDGE as f64
            + tf.translation.z as f64,
    );
    let altitude = (cam - earth_c).length() - globe.radius as f64;
    if altitude < QT_MIN_ALT {
        return; // keep loaded tiles (hidden) for an instant return to space
    }
    globe.select_timer -= time.delta_secs();
    if globe.select_timer > 0.0 {
        return;
    }
    globe.select_timer = QT_SELECT_INTERVAL;

    let (fov, h_px) = {
        let fov = match projection {
            Projection::Perspective(p) => p.fov,
            _ => std::f32::consts::FRAC_PI_4,
        };
        let h = windows.iter().next().map(|w| w.height()).unwrap_or(800.0);
        (fov, h)
    };
    let fwd = (tf.rotation * Vec3::NEG_Z).as_dvec3();
    // Generous view cone: off-screen tiles aren't refined (but stay at their
    // current level), so a spin doesn't stream max-zoom for the whole limb.
    let view_cone = (fov as f64) * 1.1;

    let mut desired: Vec<TileKey> = Vec::new();
    let n0 = 1i64 << QT_MIN_Z;
    let mut stack: Vec<TileKey> = (0..n0 * n0)
        .map(|i| TileKey::new(i % n0, i / n0, QT_MIN_Z))
        .collect();
    while let Some(key) = stack.pop() {
        let c_lat = tile_frac_to_lat(key.y as f64 + 0.5, key.z);
        let c_lng = tile_frac_to_lng(key.x as f64 + 0.5, key.z);
        let centre = geodetic_to_local(&frame, c_lat, c_lng, 0.0).as_dvec3();
        // Far-side cull, with slack so limb tiles survive.
        let normal = (centre - earth_c).normalize_or_zero();
        let to_cam = (cam - centre).normalize_or_zero();
        if normal.dot(to_cam) < -0.15 {
            continue;
        }
        // Projected size: ground edge over distance, mapped to pixels.
        let gs = tile_to_meters(key.z, c_lat);
        let d = (cam - centre).length().max(1.0);
        let px = (gs / d) as f32 * h_px / fov;
        // In-view test for the split gate: the view cone widened by the tile's
        // own angular radius. Testing only the centre against the cone broke
        // the split chain whenever a coarse tile was larger than the view (its
        // centre off-screen while the tile sat under the camera) — fine levels
        // then never loaded and the view fell back to the backstop.
        let view_angle = ((centre - cam).normalize_or_zero().dot(fwd)).clamp(-1.0, 1.0).acos();
        let ang_radius = ((gs * 0.75) / d).atan();
        let in_view = view_angle < view_cone + ang_radius;
        if px > QT_SPLIT_PX && key.z < QT_MAX_Z && in_view && desired.len() + stack.len() < QT_MAX_TILES {
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                stack.push(TileKey::new(key.x * 2 + dx, key.y * 2 + dy, key.z + 1));
            }
        } else {
            desired.push(key);
        }
    }

    // Expand each leaf with its FULL ancestor chain (leaf → parent → … → z2).
    // Rendered stacked, coarsest first, so while an uncached fine tile loads the
    // view degrades one level at a time (z10 → z9 → … → z6) instead of jumping
    // straight to the coarse floor. The mid-zoom ancestors (z5–z8) are cheap and
    // usually already on disk, and being coarser they load FIRST under the
    // coarse-first queue — so a sharp-ish fallback appears almost immediately
    // even when the leaf level is uncached and the tile server is throttling.
    // This is the fix for the "fuzzy at 120–170 km" band: there the leaf zoom is
    // ~z9–z10 (partly uncached), and previously the only fallback was the very
    // blurry z3 floor.
    let mut desired_set: HashSet<TileKey> = HashSet::new();
    for leaf in &desired {
        let mut k = leaf.clone();
        loop {
            if !desired_set.insert(k.clone()) {
                break; // this tile — and hence all its ancestors — already added
            }
            if k.z <= QT_MIN_Z {
                break;
            }
            k = TileKey::new(k.x >> 1, k.y >> 1, k.z - 1);
        }
    }
    // Permanent whole-planet floor (z2/z3), for regions outside the current view
    // (the limb) that have no leaf and thus no ancestor chain.
    for z in QT_MIN_Z..=QT_BASE_Z {
        let n = 1i64 << z;
        for y in 0..n {
            for x in 0..n {
                desired_set.insert(TileKey::new(x, y, z));
            }
        }
    }

    // Publish the fresh desired set to the workers first, so already-queued
    // jobs for tiles that just left the view get cancelled, not fetched.
    if let Ok(mut w) = globe.wanted_shared.lock() {
        *w = desired_set.clone();
    }

    // Fetch what's missing — coarse first, nearest first within a level — under
    // a per-pass budget: a steady trickle instead of 100-tile bursts (which
    // trip the tile server's rate limiting and made fetches fail en masse).
    // Backed-off failures retry after a cooldown.
    let mut queue_order: Vec<(TileKey, u64)> = desired_set
        .iter()
        .map(|k| {
            let c_lat = tile_frac_to_lat(k.y as f64 + 0.5, k.z);
            let c_lng = tile_frac_to_lng(k.x as f64 + 0.5, k.z);
            let centre = geodetic_to_local(&frame, c_lat, c_lng, 0.0).as_dvec3();
            (k.clone(), (cam - centre).length() as u64)
        })
        .collect();
    queue_order.sort_by_key(|(k, dist)| (k.z, *dist));
    let queue_order: Vec<TileKey> = queue_order.into_iter().map(|(k, _)| k).collect();
    let mut budget = QT_FETCH_BUDGET.saturating_sub(globe.pending.len());
    let mut queued = 0u32;
    if let Some(jobs) = globe.jobs.clone() {
        let gen = globe.built_gen;
        for key in &queue_order {
            if budget == 0 {
                break;
            }
            if globe.loaded.contains_key(key) || globe.pending.contains(key) {
                continue;
            }
            if let Some((attempts, last)) = globe.failed.get(key) {
                if *attempts >= QT_MAX_ATTEMPTS {
                    if last.elapsed() < QT_RETRY_AFTER {
                        continue;
                    }
                    globe.failed.remove(key); // cooldown over: fresh attempts
                }
            }
            globe.pending.insert(key.clone());
            let _ = jobs.send((key.clone(), gen));
            queued += 1;
            budget -= 1;
        }
    }
    if queued > 0 {
        tracing::info!(
            queued,
            desired = desired_set.len(),
            loaded = globe.loaded.len(),
            despawn_pending = globe.despawn_queue.len(),
            "globe tile fetches queued"
        );
    }

    // Despawn tiles no longer desired — but only once every desired tile that
    // overlaps them is loaded, so zooming never opens holes: the parent shows
    // until its children arrive, and children show until the parent returns.
    globe.desired = desired_set;
    let stale: Vec<TileKey> = globe
        .loaded
        .keys()
        .filter(|k| !globe.desired.contains(*k))
        .filter(|k| {
            globe
                .desired
                .iter()
                .filter(|d| covers(k, d) || covers(d, k))
                .all(|d| globe.loaded.contains_key(d))
        })
        .cloned()
        .collect();
    for key in stale {
        if let Some(e) = globe.loaded.remove(&key) {
            // Queue rather than despawn inline: on a fast zoom-out many tiles go
            // stale at once, and the per-frame drain keeps the teardown bounded.
            globe.despawn_queue.push(e);
        }
    }
}
