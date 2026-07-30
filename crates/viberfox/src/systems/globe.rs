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
use std::sync::{Arc, Mutex};

use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use bevy::asset::RenderAssetUsages;
use bevy::render::render_resource::TextureFormat;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::pbr::{AtmosphereMode, AtmosphereSettings};
use bevy_image::{Image, ImageSampler};
use big_space::prelude::{BigSpace, GridCell};

use crate::platform::stream::{CellStream, Policy};
use crate::resources::OsmTileUrlTemplate;
use crate::systems::map_stream::MapStream;
use crate::systems::globe_ocean::{GlobeOcean, GlobeOceanGloss, GlobeSurfaceMaterial};
use crate::systems::tile_loader::fetch_globe_tile_payload;
use vibe_core::geo::TangentFrame;
use vibe_core::world::tile_to_meters;
use vibe_core::TileKey;

/// Surface-imagery albedo in deep space. The OSM basemap is a *map*: pale beige
/// land on near-white paper, reflecting ~0.8 where the real Earth averages ~0.3.
/// At face value the sunlit hemisphere sits on the tonemapper's shoulder as a
/// flat pale ball — and because the shoulder compresses highlights, stopping the
/// *camera* down barely darkens it (and dims the limb and stars with it). So the
/// darkening is a tint on the imagery itself, where it actually bites.
///
/// Crucially it's *ramped* to 1.0 at the globe threshold (see
/// [`GlobeSurfaceAlbedo`] / `update_globe_albedo`): a fixed tint made the globe
/// step a stop darker than the flat map tiles the instant it appeared, because
/// the globe replaces those tiles exactly at the boundary. Ramped, the boundary
/// is seamless and only deep space reads planetary. Surface-only, so the
/// atmosphere limb stays bright.
const GLOBE_DEEP_ALBEDO: f32 = 0.55;
/// Change in the ramped albedo worth rewriting the globe materials for.
const ALBEDO_UPDATE_STEP: f32 = 0.03;

/// Current globe surface-imagery albedo (see [`GLOBE_DEEP_ALBEDO`]): 1.0 near the
/// threshold, falling toward `GLOBE_DEEP_ALBEDO` with altitude. Read at tile spawn
/// so new tiles start at the right tint; `update_globe_albedo` keeps the live
/// materials in step.
#[derive(Resource)]
pub struct GlobeSurfaceAlbedo(pub f32);

impl Default for GlobeSurfaceAlbedo {
    fn default() -> Self {
        Self(1.0)
    }
}

/// A globe surface's *base* imagery tone (before the albedo ramp): white for map
/// tiles, the ocean tone for the backstop sphere. `update_globe_albedo` multiplies
/// it by the current [`GlobeSurfaceAlbedo`] to get each material's `base_color`.
#[derive(Component)]
pub struct GlobeSurfaceTint(pub Vec3);

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
/// …and try it again after this cooldown, with a clean attempt count
/// (rate-limited fetches heal instead of leaving permanent holes). Seconds, on the
/// caller's clock, because that is what `platform::stream` speaks — `Instant`
/// panics on wasm.
const QT_RETRY_AFTER_SECS: f64 = 20.0;
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

#[derive(Resource)]
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
    /// The requested/in-flight/failed bookkeeping, the results sink, the fetch
    /// budget and the bounded despawn queue (`platform::stream`).
    tiles: CellStream<TileKey, TileFetch>,
    /// Fetch jobs, tagged with the build generation so workers drop stale ones.
    gen_shared: Arc<AtomicU32>,
    /// The current desired set, shared with the workers: a queued job whose
    /// tile has since left the view (zoom/pan moved on) is cancelled before it
    /// touches the network.
    wanted_shared: Arc<Mutex<HashSet<TileKey>>>,
    /// Countdown to the next quadtree selection pass.
    select_timer: f32,
    /// Last selection's desired set (kept for the despawn policy between runs).
    desired: HashSet<TileKey>,
}

impl Default for GlobeState {
    fn default() -> Self {
        Self {
            entity: None,
            center_local: Vec3::ZERO,
            radius: 0.0,
            anchor_lat: 0.0,
            anchor_lng: 0.0,
            ready: false,
            built_gen: 0,
            loaded: HashMap::new(),
            tiles: CellStream::new(Policy {
                max_in_flight: QT_FETCH_BUDGET,
                min_interval_secs: 0.0,
                max_attempts: QT_MAX_ATTEMPTS,
                // No wait between the first attempts: a single dropped fetch should
                // heal on the next selection pass, not leave a hole for 20 s.
                backoff_secs: 0.0,
                backoff_doublings: 0,
                cooldown_secs: QT_RETRY_AFTER_SECS,
                despawns_per_frame: MAX_GLOBE_TILE_DESPAWNS_PER_FRAME,
            }),
            gen_shared: Arc::new(AtomicU32::new(0)),
            wanted_shared: Arc::new(Mutex::new(HashSet::new())),
            select_timer: 0.0,
            desired: HashSet::new(),
        }
    }
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

    /// Bevy-local unit direction from Earth's centre toward a geodetic point,
    /// in the current anchor frame — the inverse of [`sub_point`]. Places the
    /// space camera over a given lat/lng (the `--shot` harness uses it).
    pub fn direction_local(&self, lat: f64, lng: f64) -> Vec3 {
        if !self.ready {
            return Vec3::Y;
        }
        let frame = TangentFrame::new(self.anchor_lat, self.anchor_lng, 0.0);
        (geodetic_to_local(&frame, lat, lng, 0.0) - self.center_local).normalize_or_zero()
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

    /// Are globe tiles still being fetched or retired? Used by the `--shot`
    /// harness to capture as soon as the globe has settled (see `shot_harness`).
    pub fn streaming_busy(&self) -> bool {
        self.tiles.busy()
    }

    /// Whether the camera is high enough that the globe is shown and the local
    /// ground props are hidden — the shared test for every system that flips
    /// per-tier visibility (single source of truth; two systems disagreeing on
    /// this was a real fight, see `apply_feature_toggles`).
    pub fn in_space(&self, cell: &GridCell, tf: &Transform) -> bool {
        self.ready && self.altitude_m(cell, tf) > GLOBE_SHOW_ALT as f64
    }

    /// How far into space the view is, 0 → 1: 0 at the globe threshold, 1 by
    /// [`SPACE_FULL_ALT`], smooth in between.
    ///
    /// Everything the space tier does *by degree* — the exaggerated atmosphere,
    /// the extra bloom, the exposure that pays for the darkened globe imagery —
    /// rides this instead of the `in_space` boolean. Switching them at the
    /// threshold put a hard cut in the middle of a zoom: the frame changed
    /// brightness, haze and glare in one step. It also applied the whole
    /// exaggeration at 200 km, where the camera is *inside* the stretched shell
    /// and the map drowned in blue.
    pub fn space_factor(&self, cell: &GridCell, tf: &Transform) -> f32 {
        if !self.ready {
            return 0.0;
        }
        crate::utils::sun::smoothstep(
            GLOBE_SHOW_ALT,
            SPACE_FULL_ALT,
            self.altitude_m(cell, tf) as f32,
        )
    }

    /// Radius the viewer-centred sky shells (star field, moon) should sit at, or
    /// `None` on the ground where their built-in radius is right.
    ///
    /// Those shells are sized for a ground observer — ~150 km out, "beyond all
    /// world geometry". From orbit that is *nearer than the planet*, so the stars
    /// blended over the Earth disc and the moon quad drew in front of it. In
    /// space they follow the camera out: past the far limb (`dist + R`) with
    /// margin, and inside the far plane `camera_controls` sets there
    /// (`dist + 2.5 R`). Angular sizes are unchanged — the meshes are scaled
    /// uniformly along with the distance.
    pub fn sky_shell_radius(&self, cell: &GridCell, tf: &Transform) -> Option<f32> {
        if !self.in_space(cell, tf) {
            return None;
        }
        let dist_center = self.altitude_m(cell, tf) + self.radius as f64;
        Some(dist_center as f32 + self.radius * 1.5)
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

/// Is a quadtree tile entirely behind the horizon, and so safe to prune (with it
/// its whole subtree)? `normal` is the outward normal at the tile centre,
/// `to_cam` the unit direction from that centre to the camera.
///
/// The test is against the tile's *centre*, so the slack has to cover the tile's
/// own angular extent on the sphere — a coarse tile spans a large arc, and at low
/// orbit its centre can sit well under the camera's horizon while the part
/// directly beneath the camera is in plain view. Without that term, at ~106 km
/// the z2 root containing the camera (centre ~29° away, i.e. ~12° below the
/// horizon) was pruned at the root: nothing under the camera ever refined and the
/// globe fell back to the z2/z3 floor — the blurry band between the flat tier's
/// 50 km ceiling and a few hundred km. The log named it exactly (`desired=80`:
/// 16 + 64 floor tiles and nothing else). Same failure mode as the centre-only
/// in-view test in the selection loop, which already carries its own widening.
///
/// The fixed −0.15 keeps limb tiles alive; the span term vanishes at fine zooms,
/// so far-side z12 tiles are still culled.
fn over_the_horizon(normal: DVec3, to_cam: DVec3, ground_span_m: f64, radius: f64) -> bool {
    let span = ((ground_span_m * 0.75) / radius).min(std::f64::consts::FRAC_PI_2);
    normal.dot(to_cam) < -0.15 - span.sin()
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
/// Build a decoded tile's mipmap chain. Pure CPU work, called on the fetch workers
/// (the fetch + decode/render itself is `tile_loader::fetch_globe_tile_payload`).
fn decode_globe_tile(rgba: image::RgbaImage, has_ocean_mask: bool) -> DecodedTile {
    let (width, height) = (rgba.width(), rgba.height());
    let (data, mip_count) =
        crate::systems::rendering::build_mip_chain(width, height, rgba.into_raw());
    DecodedTile {
        data,
        width,
        height,
        mip_count,
        has_ocean_mask,
    }
}

fn covers(a: &TileKey, b: &TileKey) -> bool {
    a.z <= b.z && (b.x >> (b.z - a.z)) == a.x && (b.y >> (b.z - a.z)) == a.y
}

/// A globe tile decoded to RGBA8 with a full mipmap chain, built on the fetch
/// worker (mirrors `map_stream::DecodedTile`).
///
/// Both halves matter. **Mipmaps:** a globe tile is 256², but from orbit it covers
/// 20–80 px — sampling mip 0 there means near-random access across the whole
/// working set every frame *and* severe minification shimmer. **Off-thread:** the
/// PNG decode used to run on the main thread, up to `MAX_GLOBE_TILE_BUILDS_PER_FRAME`
/// times per frame, right in the render loop.
struct DecodedTile {
    /// Packed mip chain, all levels, smallest last.
    data: Vec<u8>,
    width: u32,
    height: u32,
    mip_count: u32,
    /// Whether the alpha channel carries a real ocean mask (`systems::globe_ocean`)
    /// rather than the opaque 255s a source without one leaves behind.
    has_ocean_mask: bool,
}

/// Outcome of one worker fetch.
enum TileFetch {
    Ok(DecodedTile),
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
    mut materials: ResMut<Assets<GlobeSurfaceMaterial>>,
    stream: Res<MapStream>,
    bigspace: Query<Entity, With<BigSpace>>,
    _tile_url: Res<OsmTileUrlTemplate>,
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
    globe.tiles.retire_all(old_tiles);
    globe.tiles.reset();
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
            MeshMaterial3d(materials.add(GlobeSurfaceMaterial {
                base: StandardMaterial {
                    base_color: Color::srgb(0.67, 0.80, 0.85),
                    perceptual_roughness: 1.0,
                    reflectance: 0.0,
                    double_sided: true,
                    cull_mode: None,
                    ..default()
                },
                // The backstop carries no imagery, so it has no ocean mask — and
                // an unbound base-colour texture samples as opaque white, which
                // would read as "all ocean" and gloss the entire planet.
                extension: GlobeOcean::unmasked(),
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
            // Base tone for the albedo ramp (see GlobeSurfaceTint): the ocean
            // colour, matching this material's base_color at albedo 1.0.
            GlobeSurfaceTint(Vec3::new(0.67, 0.80, 0.85)),
        ))
        .id();
    globe.entity = Some(entity);
    globe.anchor_lat = clat;
    globe.anchor_lng = clng;
    globe.built_gen = gen;
    globe.gen_shared.store(gen, Ordering::Relaxed);
    globe.ready = true;

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
/// This was `false` for a while, on the strength of a measurement that turned out
/// to be someone else's bug: the "Raymarched from orbit pins every frame at
/// ~1024 ms" reading is the *same* eerily constant ~1024 ms the occluded-window
/// Wayland/FIFO stall produces (see the `present_mode` note in `main.rs`) — it was
/// the compositor withholding swapchain images, not the raymarch. Measured
/// offscreen with `--shot` (which no compositor can stall), Raymarched at
/// [`GLOBE_SKY_SAMPLES`] costs a few ms a frame from orbit on this iGPU, so it
/// stays on: the planet gets its atmospheric limb back. `VIBERFOX_NO_SPACE_RAYMARCH=1`
/// forces the cheap `LookupTexture` path if a weaker GPU ever needs it.
const SPACE_ATMOSPHERE: bool = true;

/// Whether the space tier should raymarch (see [`SPACE_ATMOSPHERE`]), read once.
fn space_raymarch() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| SPACE_ATMOSPHERE && !crate::env_flag("VIBERFOX_NO_SPACE_RAYMARCH"))
}
/// Raymarch sky samples per fragment in the space tier. Held at Bevy's default:
/// measured offscreen at 200 km and 15,000 km, 16 samples costs the same as 6
/// within run-to-run noise (~2 ms over `LookupTexture` either way), so there's
/// nothing to buy by starving the integration.
const GLOBE_SKY_SAMPLES: u32 = 16;

/// Thickness of `Atmosphere::earth` (outer − inner radius), in metres.
const EARTH_ATMOSPHERE_THICKNESS: f32 = 100_000.0;
/// How much taller the atmosphere is made in the space tier.
///
/// A deliberate departure from physics. Earth's atmosphere is 100 km on a
/// 6,371 km ball — 1.5% of the radius — so on a screen where the globe spans
/// 500 px the limb is *four pixels*: correct, and reading as a drawn outline
/// rather than air. Every "Earth from orbit" render you remember exaggerates it.
/// The `Falloff::Exponential` scale is a *fraction* of the shell's thickness, so
/// raising `outer_radius` alone stretches the whole density profile with it —
/// one knob, same shape, taller and brighter.
///
/// Only applied above the globe threshold; the ground tier gets the physical
/// shell back, which is what the city-level sky and the dawn/dusk curves are
/// tuned against.
pub const SPACE_ATMOSPHERE_THICKNESS: f32 = 2.5;

/// Altitude at which the space treatment is fully applied (see
/// [`GlobeState::space_factor`]). Deliberately far out: the exaggeration exists
/// to make a limb that's a handful of pixels read, and from low orbit the limb
/// is already thick on screen — applying it there only fogs the map you came to
/// look at.
pub const SPACE_FULL_ALT: f32 = 3_000_000.0;

/// Atmosphere density multiplier at the globe threshold (`space_factor` 0). Low
/// so the aerial-perspective wash lifts off the detailed map at low orbit; it
/// ramps up toward `1/SPACE_ATMOSPHERE_THICKNESS` in deep space, where the fuller
/// column is what makes the limb glow. See `update_globe_visibility`.
const ATMOSPHERE_DENSITY_NEAR: f32 = 0.12;

/// Below this altitude the atmosphere is held at full Earth density: this same
/// planet `Atmosphere` is the city's sky dome and its dawn/dusk colour, tuned
/// against the real preset. Between here and `GLOBE_SHOW_ALT` the density fades
/// down to `ATMOSPHERE_DENSITY_NEAR` — a climb-out band, above the city sky and
/// below the globe view, where thinning nothing critical shows.
const CITY_ATMOSPHERE_ALT: f32 = 15_000.0;

/// Change in shell thickness that's worth rebuilding the scattering medium for.
const MEDIUM_REBUILD_STEP: f32 = 0.05;

/// The planet's scattering medium, whose density `update_globe_visibility`
/// re-derives by altitude (see [`ATMOSPHERE_DENSITY_NEAR`]). Built once in
/// `free_camera::setup_camera`.
#[derive(Resource)]
pub struct AtmosphereMedia {
    pub space: Handle<bevy::light::atmosphere::ScatteringMedium>,
}
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
    mut planet: Query<(&mut Transform, &mut bevy::light::Atmosphere), With<PlanetAtmosphere>>,
) {
    if !globe.ready {
        return;
    }
    let (Ok(cell), Ok((mut tf, mut atmosphere))) = (camera.single(), planet.single_mut()) else {
        return;
    };
    let edge = crate::systems::free_camera::WORLD_CELL_EDGE as f64;
    let centre = globe.center_local.as_dvec3()
        - DVec3::new(cell.x as f64, cell.y as f64, cell.z as f64) * edge;
    let new = centre.as_vec3();
    if tf.translation != new {
        tf.translation = new;
    }

    // …and give it our globe's radius, not Bevy's. Once the entity sits at the
    // true centre, the gap between the two is not a rounding error but an
    // altitude offset: `GlobeState::radius` is geocentric at the anchor
    // (6,364.5 km at 53° N) while `Atmosphere::earth` puts its ground at
    // 6,360 km, so the atmosphere believed a camera 365 m up was at 4.9 km.
    // That misplaces the tangent ray, and `sample_transmittance_lut_segment`
    // switches formula across it (`ray_intersects_ground`); the step between its
    // two branches drew a thin ring ~2.2° below the horizon, blue-selective
    // because Rayleigh extinction is. Matching the radii puts the branch back at
    // the horizon, where the two sides agree and nothing shows. `outer_radius`
    // is derived from this each frame in `update_globe_visibility`, so the
    // 100 km shell follows. A field write on the live component is safe; it is
    // adding or removing one that hangs the device.
    if atmosphere.inner_radius != globe.radius {
        atmosphere.inner_radius = globe.radius;
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
    mut planet: Query<&mut bevy::light::Atmosphere, With<PlanetAtmosphere>>,
    media: Option<Res<AtmosphereMedia>>,
    mut scattering_media: ResMut<Assets<bevy::light::atmosphere::ScatteringMedium>>,
    // Density multiplier the current `media.space` asset was built for.
    mut applied_density: Local<f32>,
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
            // Trees are owned by `vegetation::update_vegetation_lod`, which hides
            // them at the top of its altitude ramp — far below the space tier, so
            // they are already gone by the time this system would care. Two
            // visibility writers on one entity is the documented race.
            Without<crate::systems::vegetation::VegetationSurface>,
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
    let (want_mode, want_samples) = if show_globe && space_raymarch() {
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

    // Exaggerate the shell's height from orbit (see `SPACE_ATMOSPHERE_THICKNESS`).
    // Mutating the existing component's fields is safe — it's the add/remove that
    // hangs the device, not a field write.
    if let (Ok(mut planet), Some(media)) = (planet.single_mut(), media) {
        let sf = globe.space_factor(cell, tf);
        // Shell height stretches with altitude so the limb reads from deep space
        // (see `SPACE_ATMOSPHERE_THICKNESS`); at the boundary it's Earth's real
        // 100 km.
        let thickness = 1.0 + (SPACE_ATMOSPHERE_THICKNESS - 1.0) * sf;
        let want_outer = planet.inner_radius + EARTH_ATMOSPHERE_THICKNESS * thickness;
        if planet.outer_radius != want_outer {
            planet.outer_radius = want_outer;
        }
        if planet.medium != media.space {
            planet.medium = media.space.clone();
        }
        // Air density crossfades with altitude — deliberately NOT a constant
        // column. The atmosphere's inscattering washes the surface blue in
        // proportion to the air the view ray crosses, and looking straight down
        // from low orbit that's the whole depth of it. Held at full density near
        // the boundary (the old constant-column rule) it buried the detailed map
        // under a blue veil right where the flat-tile view takes over. Three
        // regimes, blended so there's no seam anywhere:
        //   * City (below `CITY_ATMOSPHERE_ALT`): full Earth density — this same
        //     Atmosphere is the city's sky dome and its dawn/dusk colour, which
        //     are tuned against the real preset.
        //   * Low orbit (climb-out → threshold): thinning to
        //     `ATMOSPHERE_DENSITY_NEAR` so the aerial-perspective wash lifts off
        //     the detailed map and it reads like the flat tiles it hands off to.
        //   * Deep space: rising to the column that makes the limb glow.
        // `city_fade` reaches 1 exactly at the globe threshold, where `sf` picks
        // up from 0, so the inner and outer lerps meet continuously.
        let alt = globe.altitude_m(cell, tf) as f32;
        let city_fade = crate::utils::sun::smoothstep(CITY_ATMOSPHERE_ALT, GLOBE_SHOW_ALT, alt);
        let density = (1.0f32.lerp(ATMOSPHERE_DENSITY_NEAR, city_fade))
            .lerp(1.0 / SPACE_ATMOSPHERE_THICKNESS, sf);
        // The medium is an asset; rewriting it rebuilds its falloff/phase LUTs, so
        // re-derive it only when the density has actually drifted (a handful of
        // times across a full zoom, not every frame).
        if (density - *applied_density).abs() > MEDIUM_REBUILD_STEP {
            if let Some(mut medium) = scattering_media.get_mut(&media.space) {
                *medium = bevy::light::atmosphere::ScatteringMedium::default()
                    .with_density_multiplier(density);
                *applied_density = density;
            }
        }
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

/// Ramp the globe's surface-imagery albedo with altitude (see [`GLOBE_DEEP_ALBEDO`]):
/// full brightness at the tier boundary, so the globe matches the flat map tiles
/// it replaces there, darkening to a planetary tone as you climb to orbit. Writes
/// the shared [`GlobeSurfaceAlbedo`] (read at tile spawn) and rewrites the live
/// tile + backstop materials when it drifts past [`ALBEDO_UPDATE_STEP`].
///
/// The ocean gloss (`systems::globe_ocean`) rides the same ramp and the same
/// throttle: both are functions of `space_factor`, and they must reach the tier
/// boundary together — the flat map tiles just below it are matte and at full
/// albedo, so anything that arrives early puts a step in the middle of a climb.
pub fn update_globe_albedo(
    globe: Res<GlobeState>,
    camera: Query<
        (&GridCell, &Transform),
        With<crate::systems::free_camera::FreeCamera>,
    >,
    mut albedo: ResMut<GlobeSurfaceAlbedo>,
    mut gloss: ResMut<GlobeOceanGloss>,
    tinted: Query<(&GlobeSurfaceTint, &MeshMaterial3d<GlobeSurfaceMaterial>), With<GlobeMesh>>,
    mut materials: ResMut<Assets<GlobeSurfaceMaterial>>,
    mut applied: Local<Option<f32>>,
) {
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let space = globe.space_factor(cell, tf);
    let a = 1.0f32.lerp(GLOBE_DEEP_ALBEDO, space);
    albedo.0 = a;
    gloss.0 = space;
    if applied.is_some_and(|p| (a - p).abs() < ALBEDO_UPDATE_STEP) {
        return;
    }
    *applied = Some(a);
    for (tint, mat) in tinted.iter() {
        if let Some(mut m) = materials.get_mut(&mat.0) {
            m.base.base_color = Color::srgb(tint.0.x * a, tint.0.y * a, tint.0.z * a);
            // Only the masked surfaces have an ocean at all; leave the backstop
            // sphere's inert extension alone.
            if m.extension.ocean.y > 0.0 {
                m.extension.ocean.x = space;
            }
        }
    }
}

/// Whole-globe quadtree tile streaming: select the tile set whose screen-space
/// size is right for the current view (splitting down to `QT_MAX_Z`), fetch the
/// missing ones (coarse first), and swap tiles in/out without holes — a parent
/// stays until all four children are ready, and vice versa on zoom-out.
#[allow(clippy::too_many_arguments)]
/// Spawn one globe tile fetch+decode task (ADR-033 seam), replacing an iteration
/// of the old worker pool: gen-tagged (a re-anchor drops stale jobs) and wanted-
/// checked (a tile that left the view is cancelled before hitting the network).
#[allow(clippy::too_many_arguments)]
fn spawn_globe_tile(
    key: TileKey,
    job_gen: u32,
    template: String,
    results: Arc<Mutex<Vec<(TileKey, TileFetch)>>>,
    live_gen: Arc<AtomicU32>,
    wanted: Arc<Mutex<HashSet<TileKey>>>,
) {
    IoTaskPool::get()
        .spawn(async move {
            if job_gen != live_gen.load(Ordering::Relaxed) {
                return;
            }
            let still_wanted = wanted.lock().map(|w| w.contains(&key)).unwrap_or(true);
            if !still_wanted {
                if let Ok(mut g) = results.lock() {
                    g.push((key, TileFetch::Cancelled));
                }
                return;
            }
            // Fetch here, decode on the worker pool (ADR-033). The rasterise +
            // ocean mask + mip chain is the whole cost of a globe tile and up to
            // `QT_FETCH_BUDGET` of them are in flight at once; on wasm this future
            // runs on the main thread, so doing that here was the frame loop.
            match fetch_globe_tile_payload(&key, &template).await {
                Ok(payload) => {
                    crate::platform::workers::submit(Box::new(move || {
                        let outcome = match payload.decode() {
                            Ok((rgba, masked)) => TileFetch::Ok(decode_globe_tile(rgba, masked)),
                            Err(_) => TileFetch::Failed,
                        };
                        if let Ok(mut g) = results.lock() {
                            g.push((key, outcome));
                        }
                    }));
                }
                Err(_) => {
                    if let Ok(mut g) = results.lock() {
                        g.push((key, TileFetch::Failed));
                    }
                }
            }
        })
        .detach();
}

pub fn update_globe_tiles(
    mut commands: Commands,
    mut globe: ResMut<GlobeState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<GlobeSurfaceMaterial>>,
    tile_url: Res<OsmTileUrlTemplate>,
    camera: Query<
        (&GridCell, &Transform, &Projection),
        With<crate::systems::free_camera::FreeCamera>,
    >,
    windows: Query<&Window>,
    headless: Option<Res<crate::resources::HeadlessSurface>>,
    bigspace: Query<Entity, With<BigSpace>>,
    surface_albedo: Res<GlobeSurfaceAlbedo>,
    ocean_gloss: Res<GlobeOceanGloss>,
    time: Res<Time>,
) {
    let headless = headless.map(|h| *h);
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
    globe.tiles.drain_despawns(&mut commands);
    let now_secs = time.elapsed_secs_f64();

    // ── Harvest finished fetches into live tile entities (every frame) ──────
    let ready = globe.tiles.take_results();
    let mut fetch_failures = 0u32;
    let mut built = 0usize;
    let mut deferred: Vec<(TileKey, TileFetch)> = Vec::new();
    for (key, outcome) in ready {
        let tile = match outcome {
            // Left the view while queued: forget it so re-wanting it re-fetches
            // (the bytes are cached, so that is cheap).
            TileFetch::Cancelled => {
                globe.tiles.forget(&key);
                continue;
            }
            TileFetch::Failed => {
                globe.tiles.failed(&key, now_secs);
                fetch_failures += 1;
                continue;
            }
            TileFetch::Ok(tile) => tile,
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
            globe.tiles.forget(&key);
            continue;
        }
        // Cap decode + upload per frame; defer the rest (still counted in flight, so
        // they aren't re-fetched *and* the budget keeps bounding how many decoded
        // tiles are held) to keep the burst from overrunning the GPU timeout.
        if built >= MAX_GLOBE_TILE_BUILDS_PER_FRAME {
            deferred.push((key, TileFetch::Ok(tile)));
            continue;
        }
        globe.tiles.succeeded(&key);
        let has_ocean_mask = tile.has_ocean_mask;
        // Decode + mips already done on the worker; just assemble and upload.
        let image = crate::systems::rendering::image_from_mip_chain(
            tile.width,
            tile.height,
            tile.data,
            tile.mip_count,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::default(),
            ImageSampler::linear(),
        );
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
        // Web Mercator stops at ±85.05°, so the tile grid leaves a bare cap at
        // each pole — the backstop sphere showing through as a disc from orbit.
        // Web maps close it by stretching the outermost row of the top/bottom
        // tiles onto the pole itself; the cap's imagery is a smear of the last
        // texel row, which is ice/ocean at both poles. The row collapses to a
        // point at the pole, turning that band of quads into a triangle fan.
        let rows = 1i64 << key.z;
        for iy in 0..=QT_SEGS {
            let v = iy as f64 / QT_SEGS as f64;
            let lat = if key.y == 0 && iy == 0 {
                90.0
            } else if key.y == rows - 1 && iy == QT_SEGS {
                -90.0
            } else {
                tile_frac_to_lat(key.y as f64 + v, key.z)
            };
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
                MeshMaterial3d(materials.add(GlobeSurfaceMaterial {
                    base: StandardMaterial {
                        // Current ramped albedo, so a tile streaming in while
                        // parked in orbit starts at the right tint instead of
                        // popping bright (the throttled `update_globe_albedo`
                        // won't re-touch it if the camera isn't moving). White
                        // base tone (see the tint).
                        base_color: {
                            let a = surface_albedo.0;
                            Color::srgb(a, a, a)
                        },
                        base_color_texture: Some(texture),
                        perceptual_roughness: 1.0,
                        reflectance: 0.0,
                        ..default()
                    },
                    // Same contract as the albedo above: come up at the ocean
                    // gloss the rest of the globe is already showing.
                    extension: if has_ocean_mask {
                        GlobeOcean::masked(ocean_gloss.0)
                    } else {
                        GlobeOcean::unmasked()
                    },
                })),
                Transform::from_translation(origin),
                GridCell::default(),
                ChildOf(root),
                NoFrustumCulling,
                bevy::light::NotShadowCaster,
                bevy::light::NotShadowReceiver,
                Visibility::Hidden, // update_globe_visibility shows it in space
                GlobeMesh,
                GlobeSurfaceTint(Vec3::ONE),
            ))
            .id();
        if let Some(old) = globe.loaded.insert(key, entity) {
            globe.tiles.retire(old);
        }
        built += 1;
    }
    // Requeue the tiles we deferred this frame for the next one.
    globe.tiles.defer(deferred);
    if fetch_failures > 0 {
        // A run of failures usually means the tile server is rate-limiting;
        // failed tiles retry after a cooldown (QT_RETRY_AFTER_SECS).
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
        // Headless (`--shot`) has no window; the offscreen target is the viewport,
        // and getting its height wrong mis-sizes every screen-space split test.
        let h = windows
            .iter()
            .next()
            .map(|w| w.height())
            .or_else(|| headless.map(|h| h.size.y as f32 / h.scale))
            .unwrap_or(800.0);
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
        let gs = tile_to_meters(key.z, c_lat);
        let normal = (centre - earth_c).normalize_or_zero();
        let to_cam = (cam - centre).normalize_or_zero();
        if over_the_horizon(normal, to_cam, gs, globe.radius as f64) {
            continue;
        }
        // Projected size: ground edge over distance, mapped to pixels.
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
    // Filter *before* the map: the consuming loop below skips anything already
    // loaded or in flight, so in steady state nearly every one of the ~900 desired
    // keys was paying a `sinh` + `atan` + a full geodetic→ENU transform only to be
    // discarded. Sort on the squared distance — same rank order, ~900 fewer sqrts.
    let mut queue_order: Vec<(TileKey, u64)> = desired_set
        .iter()
        .filter(|k| !globe.loaded.contains_key(*k) && !globe.tiles.is_requested(k))
        .map(|k| {
            let c_lat = tile_frac_to_lat(k.y as f64 + 0.5, k.z);
            let c_lng = tile_frac_to_lng(k.x as f64 + 0.5, k.z);
            let centre = geodetic_to_local(&frame, c_lat, c_lng, 0.0).as_dvec3();
            (*k, (cam - centre).length_squared() as u64)
        })
        .collect();
    queue_order.sort_by_key(|(k, dist)| (k.z, *dist));
    // Each pass re-decides the whole order, so drop whatever the last one queued
    // and did not start: a stale queue would outrank this fresh coarse-first,
    // nearest-first order and spend the budget on tiles the view has moved past.
    // `request` then de-dups and holds the attempt/cooldown policy, and `next_job`
    // enforces QT_FETCH_BUDGET — so the budget is the queue's, not a counter here.
    globe.tiles.clear_queue();
    for (key, _) in &queue_order {
        globe.tiles.request(*key, now_secs);
    }
    let mut queued = 0u32;
    {
        let gen = globe.built_gen;
        let results = globe.tiles.sink();
        let live_gen = globe.gen_shared.clone();
        let wanted = globe.wanted_shared.clone();
        let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
        while let Some(key) = globe.tiles.next_job(now_secs) {
            spawn_globe_tile(
                key,
                gen,
                template.clone(),
                results.clone(),
                live_gen.clone(),
                wanted.clone(),
            );
            queued += 1;
        }
    }
    if queued > 0 {
        tracing::info!(
            queued,
            desired = desired_set.len(),
            loaded = globe.loaded.len(),
            in_flight = globe.tiles.in_flight(),
            despawn_pending = globe.tiles.despawn_backlog(),
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
            globe.tiles.retire(e);
        }
        // The texture dies with the entity, so a tile that comes back must fetch
        // again; leaving it `requested` would suppress that.
        globe.tiles.forget(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: f64 = 6_378_137.0;

    /// Outward normal and camera direction for a tile whose centre lies
    /// `theta_deg` of arc from the sub-camera point, with the camera at
    /// `alt_m`. The tile centre sits at the sphere's +Z pole of this frame.
    fn geometry(theta_deg: f64, alt_m: f64) -> (DVec3, DVec3) {
        let centre = DVec3::new(0.0, 0.0, R);
        let (s, c) = theta_deg.to_radians().sin_cos();
        let cam = DVec3::new((R + alt_m) * s, 0.0, (R + alt_m) * c);
        (centre.normalize(), (cam - centre).normalize())
    }

    /// The regression: at low orbit the coarse root under the camera has its
    /// centre far below the horizon, but it must NOT be pruned — everything
    /// beneath the camera refines out of it.
    #[test]
    fn coarse_tile_under_a_low_orbit_camera_survives() {
        // z2 over Europe, camera at ~106 km: centre ~29° away (~12° below the
        // camera's horizon as seen from the tile).
        let (normal, to_cam) = geometry(29.0, 106_000.0);
        assert!(
            normal.dot(to_cam) < -0.15,
            "test premise: the centre must be past the plain −0.15 cut"
        );
        let z2_span = tile_to_meters(2, 41.0);
        assert!(!over_the_horizon(normal, to_cam, z2_span, R));
    }

    /// …while a fine tile at the same spot is genuinely over the horizon and
    /// still gets culled: the widening must not disable the cull outright.
    #[test]
    fn fine_tile_over_the_horizon_is_still_culled() {
        let (normal, to_cam) = geometry(29.0, 106_000.0);
        let z12_span = tile_to_meters(12, 41.0);
        assert!(over_the_horizon(normal, to_cam, z12_span, R));
    }

    /// A tile straight under the camera is never culled, at any zoom.
    #[test]
    fn tile_beneath_the_camera_is_never_culled() {
        for z in [2, 6, 12] {
            for alt in [60_000.0, 1_000_000.0, 30_000_000.0] {
                let (normal, to_cam) = geometry(0.0, alt);
                assert!(!over_the_horizon(normal, to_cam, tile_to_meters(z, 0.0), R));
            }
        }
    }
}
