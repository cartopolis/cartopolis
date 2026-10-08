//! Planetary globe / space tier (ADR-025).
//!
//! A true-scale WGS84 Earth built in the sim anchor's ENU tangent frame
//! (`cartopolis_core::geo`), so the flat streamed-tile city view sits on the sphere's
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

use crate::components::{GlobeMesh, HideInSpace, PlanetAtmosphere};
use crate::platform::sync::{Arc, Mutex};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::math::DVec3;
use bevy::pbr::{AtmosphereMode, AtmosphereSettings};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::tasks::IoTaskPool;
use bevy_image::Image;
use big_space::prelude::{BigSpace, GridCell};

use crate::platform::stream::{CellStream, Policy};
use crate::resources::OsmTileUrlTemplate;
use crate::systems::globe_ocean::{GlobeOcean, GlobeOceanGloss, GlobeSurfaceMaterial};
use crate::systems::map_stream::MapStream;
use crate::systems::tile_loader::fetch_globe_tile_payload;
use cartopolis_core::geo::TangentFrame;
use cartopolis_core::world::tile_to_meters;
use cartopolis_core::TileKey;

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
///
/// **1.0 since 2026-09-27, i.e. no darkening.** 0.55 was set against the
/// paper-white palette. The land tones now come from aerial imagery and the
/// land cover (`vector_tiles::palette`, `landcover_fills`), so they are already
/// the ground's real albedo, and any tint on top only sank them further under
/// the atmosphere's blue: measured on the Deck from 14,000 km, the Sahara drew
/// (133,126,138) at 0.75 — bluer than it was red. Kept, ramp and all, so a
/// brighter basemap source can still be damped here.
const GLOBE_DEEP_ALBEDO: f32 = 1.0;
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
///
/// Halved by the phone tier (`quality::Budget::per_frame`), which is not what that
/// helper is named for but is the same arithmetic and the same reason: 320 leaves
/// plus ancestors is ~450 draws of planet, issued from the single thread the web
/// build has. The globe is the coarsest thing on screen, so it is also the
/// cheapest place to give detail back.
const QT_MAX_TILES: usize = 320;
/// Per-zoom outward lift (m): finer tiles sit slightly above coarser ones, so
/// parents showing through while children load never z-fight.
///
/// This is the *geometric* half of the separation and it stops working somewhere
/// around 700 km. The space tier's depth range is enormous (`free_camera`'s
/// `near = altitude·0.01`, `far` past the limb), so with reverse-Z f32 the depth
/// quantum at the surface is ~`d²/near·2⁻²⁴`: about 0.4 m at 60 km, but ~6 m at
/// 1,000 km and ~120 m at 10,000 km — by which point the whole z2→z12 lift ladder
/// fits inside a single quantum and every level of the stack z-fights every
/// other. Raising the lift to cover that is not an option either: the metres it
/// would take from orbit are a visible step against the flat tiles at the tier
/// boundary. See [`QT_DEPTH_BIAS_PER_Z`] for the half that does scale.
const QT_LIFT_PER_Z: f32 = 4.0;

/// How far the whole quadtree sits **below** the true surface (m).
///
/// The lift ladder above is measured outward from the sphere, so the globe's
/// finest tiles stand `QT_MAX_Z · QT_LIFT_PER_Z` = 48 m proud of it. That was
/// free while the globe only ever appeared above 50 km with every flat tile
/// hidden. Now that both tiers are drawn together (see [`GLOBE_DRAW_ALT`]) it is
/// the difference between the planet filling in past the clipmap and the planet
/// covering the city, so the ladder is offset until its top rung clears the
/// clipmap's own resting heights with room to spare.
///
/// A constant, and applied to every zoom, so it changes nothing *within* the
/// globe: the ladder keeps its rungs and the sphere is uniformly 68 m smaller.
/// What it costs is a step at the seam where the clipmap ends — 68 m at 82 km,
/// which is 1.5 px of a 1080-tall frame at the altitude the seam is visible
/// from, against the alternative of a blank planet over the map.
const QT_SURFACE_SINK: f64 = 68.0;
/// Per-zoom depth bias, in the same units as every other bias in the scene
/// (`ground_stack::DEPTH_BIAS_STEP`).
///
/// **A separate ladder, deliberately.** `ground_stack::GroundLayer` orders the
/// sheets of the *ground* tier against each other; this orders the globe
/// quadtree's zoom levels against each other, and the two are never on screen
/// coplanar — the globe carries `HideInSpace`'s opposite and the flat tiles are
/// gone by the time it is drawn. Sharing the unit is what makes them comparable;
/// sharing the ladder would mean one list holding two things that never meet.
///
/// `StandardMaterial::depth_bias` becomes wgpu's `DepthBiasState::constant`,
/// which is denominated in *depth-buffer* units rather than metres. That is
/// exactly what [`QT_LIFT_PER_Z`] cannot be: one rung per zoom level holds the
/// stack apart at 60 km and at 35,000 km alike, with no world-space step at the
/// tier boundary to pay for it.
const QT_DEPTH_BIAS_PER_Z: f32 = crate::systems::vector_tiles::DEPTH_BIAS_STEP;

/// Edge length a globe tile is rasterised and stored at.
///
/// Deliberately **not** `vector_tiles::TILE_PX` (512), which is sized for a flat
/// tile stretched across a region on the ground. A globe tile is capped at
/// `QT_SPLIT_PX` on screen — the split rule means a leaf is only ever 170–340 px
/// — so 512 is 2–3× oversampled per axis and buys nothing anyone can see.
///
/// It is the quadtree's dominant cost in both directions. Scan conversion is
/// per-covered-pixel, so this is ~4× off the ~121 ms `render_mvt`
/// (`docs/notes/tile-raster-cost.md`) that every one of several hundred tiles pays
/// on the worker pool; and the texture lives for as long as the view holds the
/// tile, so it is also ~4× off a resident set that ran to ~650 MB at low orbit.
///
/// This was 256 once, by accident of the raster basemap the globe was written
/// against — `DecodedTile`'s doc comment still said "a globe tile is 256²" long
/// after the vector switch silently made it 512². Restoring it is a restoration,
/// not a new trade-off.
pub const GLOBE_TILE_PX: u32 = 256;

/// [`RenderAssetUsages`] for the globe's meshes — the backstop sphere and every
/// tile patch.
///
/// `RENDER_WORLD` **without** `MAIN_WORLD`, which is not the default. The default
/// keeps the vertex buffers alive in `Assets<Mesh>` for the lifetime of the handle
/// *as well as* on the GPU, so every mesh is paid for twice — the identical
/// mistake `tile_loader::TILE_TEXTURE_USAGE` documents for the tile *textures*,
/// and fixed there, while the meshes it sits next to kept the default. It is not
/// a rounding error at this count: a tile patch is `qt_segs(z)`² vertices of
/// position + normal + UV plus its index buffer, and `globe-tile-cost.md` has
/// several hundred of them live across the 60 km–1,000 km band, on top of the
/// backstop sphere's own `GLOBE_RINGS × GLOBE_SEGMENTS` grid.
///
/// Nothing reads a globe mesh back. There is no mesh raycast anywhere in this
/// tree — `picking` intersects an analytic plane and an analytic box
/// (`ray_plane_y`, `ray_unit_box`) and never opens `Assets<Mesh>` — and these
/// vertices are only ever drawn. The failure mode if that ever changes is the one
/// `TILE_TEXTURE_USAGE` names, and it is silent: a consumer that wants CPU data
/// finds it gone once the asset has been prepared. So a future raycast against the
/// globe means changing this constant, not adding a lookup.
const GLOBE_MESH_USAGE: RenderAssetUsages = RenderAssetUsages::RENDER_WORLD;

/// Minimum mesh tessellation of one tile patch (segments per axis).
///
/// Enough for any tile whose own angular span is small — which, past about z5, is
/// all of them. Coarse tiles get more; see [`qt_segs`].
const QT_SEGS: usize = 12;

/// Ceiling on the tessellation [`qt_segs`] will hand out, so a coarse tile cannot
/// cost an unbounded mesh. 64 segments is 4,225 vertices, and there are only ever
/// 16 tiles at z2 and 64 at z3.
const QT_SEGS_MAX: usize = 64;

/// How far inside the true sphere a tile patch's facets are allowed to sag.
///
/// This is the whole reason [`qt_segs`] exists. A flat quad chorded across an arc
/// of `θ` radians sits `≈ R·θ²/8` below the sphere at its middle, and at a fixed
/// 12 segments a z2 tile's facets span 7.5° — **14 km** of sag, over facets 830 km
/// wide. From just above the tier boundary that is not a sphere at all: the
/// visible globe is a handful of enormous flat panels meeting at angles, which
/// reads exactly like a flat tile grid, and it is what you see on a fast zoom-out,
/// when the coarse floor is all that has arrived. 500 m is below a pixel at any
/// altitude the globe is drawn at.
const QT_MAX_SAG_M: f64 = 500.0;

/// Segments per axis for a tile patch at `z`, chosen so no facet sags more than
/// [`QT_MAX_SAG_M`] inside the sphere.
///
/// Inverting `sag ≈ R·(span/segs)²/8` gives `segs ≥ span·√(R/(8·sag))`, rounded up
/// to a power of two so the vertex grids of the refined levels halve cleanly into
/// one another. Equatorial span is the widest a tile at this zoom can be, so this
/// is the conservative answer everywhere.
///
/// The rounding happens **before** the [`QT_SEGS`] floor, not after: rounding
/// afterwards turns the floor itself into 16 and quietly hands every fine tile a
/// bigger grid than it needs — which, fine tiles being nearly the whole live set,
/// is where the vertex budget would actually have gone.
fn qt_segs(z: u32) -> usize {
    let span = std::f64::consts::TAU / f64::from(1u32 << z);
    let needed = span * (EARTH_RADIUS_M / (8.0 * QT_MAX_SAG_M)).sqrt();
    (needed.ceil() as usize)
        .next_power_of_two()
        .clamp(QT_SEGS, QT_SEGS_MAX)
}

/// Mean Earth radius, for the tessellation arithmetic above. The globe's own
/// `radius` is derived from the anchor frame and is not available where the mesh
/// grid is sized.
const EARTH_RADIUS_M: f64 = 6_371_000.0;
/// How far below the surface the ocean backstop sphere sits. Tile patches are
/// flat-faceted, so a tile's interior sags inside the true sphere; a backstop at
/// the surface poked through those interiors (tiles rendered as vertex-diamonds
/// with ocean showing between), so it is sunk below the worst case and only shows
/// where no tile exists at all (poles, not-yet-loaded areas).
///
/// The worst case used to be ~14 km, at z2's fixed 12 segments. [`qt_segs`] now
/// holds it to [`QT_MAX_SAG_M`], so this has two orders of magnitude of margin it
/// no longer needs — left alone deliberately, since the only thing the slack costs
/// is that the backstop shows through slightly less often than it could.
const BACKSTOP_DEPTH: f64 = 50_000.0;
/// Quadtree selection/streaming only runs above this altitude (below it the
/// globe is not drawn and the flat tiles own the whole view); loaded tiles are
/// kept.
///
/// Tied to [`GLOBE_DRAW_ALT`] rather than set independently: it used to sit at
/// 35 km, comfortably under the 50 km the globe appeared at, so the quadtree had
/// a head start on the handoff. Now that the globe is drawn from 2 km the two
/// are the same question — refining a planet nobody is shown is the waste the
/// floor exists to prevent, and *not* refining one that is on screen is a hole
/// in the horizon.
const QT_MIN_ALT: f64 = GLOBE_DRAW_ALT as f64;

/// Max globe tiles to build (mesh + texture upload) per frame. A zoom-out can
/// land 100+ fetched tiles at once; building them all in one frame packs one
/// giant command buffer that overran a weak GPU's timeout
/// (`VK_ERROR_DEVICE_LOST`). The rest are deferred to later frames — they stay in
/// `pending` (so they aren't re-fetched) and are pushed back into `results`.
///
/// The guard is really a *bytes per frame* budget; this is its per-tile proxy, so
/// it moves when the tile size does. At the old 512² a tile was ~1.4 MB of packed
/// mip chain and 4 of them ~5.6 MB; at [`GLOBE_TILE_PX`] it is ~350 KB, so 8 is
/// half the upload burst the cap was set to survive while draining a backlog
/// twice as fast — which is most of what a zoom through the tier feels like.
const MAX_GLOBE_TILE_BUILDS_PER_FRAME: usize = 8;

/// Tile materials the albedo ramp may rewrite per frame ([`update_globe_albedo`]).
///
/// The third burst in the same frames as the two caps around it, and the only one
/// that had none. A step touches every live tile — ~450 at low orbit — dirtying a
/// uniform apiece for re-upload, and it fires while a climb has the build and
/// despawn queues at their own limits.
///
/// Higher than those two because a uniform write is not a mesh upload: this is a
/// 64-byte buffer patch, not a megabyte of packed mips. 64 clears a full set in
/// about seven frames, which is well inside the time `ALBEDO_UPDATE_STEP` (0.03 of
/// a ramp that takes a whole climb to traverse) allows before the next step.
const MAX_GLOBE_ALBEDO_WRITES_PER_FRAME: usize = 64;

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
                despawns_per_frame: crate::quality::budget()
                    .per_frame(MAX_GLOBE_TILE_DESPAWNS_PER_FRAME),
            }),
            gen_shared: Arc::new(AtomicU32::new(0)),
            wanted_shared: Arc::new(Mutex::new(HashSet::new())),
            select_timer: 0.0,
            desired: HashSet::new(),
        }
    }
}

impl crate::systems::stream_watch::StreamBusy for GlobeState {
    const NAME: &'static str = "globe";
    fn stream_busy(&self) -> bool {
        self.streaming_busy()
    }
}

impl GlobeState {
    /// Quadtree tiles applied, and how many this view wants — the boot splash's
    /// readiness signal and progress track *above* the flat streamer's freeze
    /// altitude, where `MapStream` is deliberately doing nothing at all.
    ///
    /// The pair is honest in a way the flat streamer's cannot be without a query:
    /// [`Self::loaded`] holds only tiles whose texture is on, so the numerator is
    /// what is actually on screen.
    pub fn tile_progress(&self) -> (usize, usize) {
        (self.loaded.len(), self.desired.len())
    }

    /// The camera's **geocentric** altitude (m): distance from the Earth's centre
    /// minus the surface radius. This is the only correct "how high am I" once
    /// the camera can orbit the planet — the local-frame `y` is only altitude
    /// near the anchor and goes *negative* on the far side of the globe, which
    /// made every y-based check (tile freeze, globe visibility, fog, skybox)
    /// misfire in space. Falls back to local `y` until the globe exists.
    pub fn altitude_m(&self, cell: &GridCell, tf: &Transform) -> f64 {
        let edge = crate::grid::WORLD_CELL_EDGE as f64;
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

    /// The build this sphere belongs to — bumped whenever [`setup_globe`] rebuilds
    /// in a new tangent frame. Consumers that bake `drop_at` into a mesh compare
    /// it to know when their geometry is stale, exactly as they do with
    /// `TerrainField::revision`.
    pub fn build_revision(&self) -> u32 {
        if self.ready {
            self.built_gen
        } else {
            u32::MAX
        }
    }

    /// How far the Earth's surface has fallen away from the anchor's tangent
    /// plane, `horizontal_m` metres out from the anchor (metres, ≥ 0).
    ///
    /// This is what makes the flat tier a *patch of the globe* rather than a
    /// plane that happens to be near it. [`setup_globe`] builds the sphere in the
    /// anchor's tangent frame with the anchor exactly on its surface at the local
    /// origin, so `y = -drop_at(d)` is not an approximation of the globe — it is
    /// the globe, evaluated in the flat frame the basemap is already placed in.
    /// Both surfaces therefore meet with no step wherever the clipmap ends, which
    /// is the whole reason the streamed tiles can be handed off to the quadtree
    /// at all.
    ///
    /// Zero until the globe is placed: the ground is flat for the second or two
    /// before an anchor exists, exactly as it always was, and every consumer adds
    /// this unconditionally (same contract as `TerrainField::at`).
    pub fn drop_at(&self, horizontal_m: f32) -> f32 {
        if !self.ready {
            return 0.0;
        }
        curvature_drop_m(horizontal_m, self.radius)
    }

    /// The outward (up) normal of that surface at the same point — the sphere
    /// normal, not `+Y`.
    ///
    /// Ground meshes need it or a tile 80 km out is lit as though it were lying
    /// under the anchor: by then the true surface has tilted 0.74°, which is
    /// small, but it is the same 0.74° for every tile in that ring and so reads
    /// as a band of flat shading rather than as ground curving into the light.
    pub fn surface_normal(&self, x: f32, z: f32) -> Vec3 {
        if !self.ready {
            return Vec3::Y;
        }
        let p = Vec3::new(x, -self.drop_at(Vec2::new(x, z).length()), z);
        (p - self.center_local).normalize_or(Vec3::Y)
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
        Vec3::new(
            frame.east.z as f32,
            frame.up.z as f32,
            -frame.north.z as f32,
        )
        .normalize_or_zero()
    }

    /// Are globe tiles still being fetched or retired? Used by the `--shot`
    /// harness to capture as soon as the globe has settled (see `shot_harness`).
    pub fn streaming_busy(&self) -> bool {
        self.tiles.busy()
    }

    /// Live tile entities, and how many the last selection asked for.
    ///
    /// `ShotMetrics` reports both because they answer different questions than
    /// `globe_entities` does. That counts `GlobeMesh` entities, which includes
    /// the backstop sphere and anything still draining the despawn queue; these
    /// two are the quadtree's own books, and the gap between them is how much of
    /// the pyramid is still arriving.
    pub fn tiles_loaded(&self) -> usize {
        self.loaded.len()
    }

    /// See [`Self::tiles_loaded`].
    pub fn tiles_desired(&self) -> usize {
        self.desired.len()
    }

    /// Resident tile-texture bytes, estimated from the tile count.
    ///
    /// Estimated rather than measured: walking `Assets<Image>` for the real
    /// figure means resolving every tile's material and handle, on the frame the
    /// shutter fires. The arithmetic is exact anyway — every globe tile is
    /// [`GLOBE_TILE_PX`]² RGBA8 with a full mip chain, which is the ⁴⁄₃ — and it
    /// is the *count* that varies.
    ///
    /// This is the column that separates the two candidate causes of the black
    /// artifacts: if they track this, it is memory pressure; if they track
    /// `alt_m` while this stays flat, it is the depth range. Nothing in the
    /// metrics could tell those apart before.
    pub fn tile_texture_bytes(&self) -> usize {
        let mip0 = (GLOBE_TILE_PX as usize).pow(2) * 4;
        self.loaded.len() * (mip0 * 4 / 3)
    }

    /// Whether the camera is high enough that the globe is shown and the local
    /// ground props are hidden — the shared test for every system that flips
    /// per-tier visibility (single source of truth; two systems disagreeing on
    /// this was a real fight, see `apply_feature_toggles`).
    pub fn in_space(&self, cell: &GridCell, tf: &Transform) -> bool {
        self.ready && self.altitude_m(cell, tf) > GLOBE_SHOW_ALT as f64
    }

    /// Whether the globe's own geometry should be drawn at all — true from
    /// [`GLOBE_DRAW_ALT`] up, i.e. for the whole space tier *and* the band below
    /// it where the flat clipmap runs out before the horizon does.
    ///
    /// Kept separate from [`Self::in_space`] on purpose. `in_space` answers "is
    /// the local scene over?" and a dozen systems ask it — hiding buildings,
    /// freezing streamers, switching the lighting model, choosing a heading
    /// readout. Only two things want this one: whether to draw the planet and
    /// whether to refine its quadtree. Folding them back together is what put a
    /// flat 150 km disc where the Earth should have been.
    pub fn drawn(&self, cell: &GridCell, tf: &Transform) -> bool {
        self.ready && self.altitude_m(cell, tf) > GLOBE_DRAW_ALT as f64
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
    ///
    /// The band below the space tier needs them moved too, for the same reason
    /// one step smaller. "Beyond all world geometry" meant the 150 km ground
    /// skirt; the world now runs to the planet's horizon, which is 391 km from
    /// 12 km up — so at night the star shell would hang between the camera and
    /// the far ground. Below `GLOBE_DRAW_ALT` nothing has changed and the
    /// built-in radius is still right.
    pub fn sky_shell_radius(&self, cell: &GridCell, tf: &Transform) -> Option<f32> {
        if self.in_space(cell, tf) {
            let dist_center = self.altitude_m(cell, tf) + self.radius as f64;
            return Some(dist_center as f32 + self.radius * 1.5);
        }
        if !self.drawn(cell, tf) {
            return None;
        }
        // Past the horizon and the far plane's own margin, so the shells sit
        // outside every piece of ground that can be drawn — and still well
        // inside the far plane itself.
        let horizon = crate::systems::free_camera::horizon_distance_m(
            self.altitude_m(cell, tf) as f32,
            self.radius,
        );
        Some((horizon * 1.35).max(SKY_SHELL_GROUND_RADIUS))
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
        cartopolis_core::ecef_to_geodetic(ecef_dir.normalize_or_zero() * 6_378_137.0);
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
    ((1.0 - 2.0 * fy / n) * std::f64::consts::PI)
        .sinh()
        .atan()
        .to_degrees()
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
    let (data, mip_count) = crate::systems::mip::build_mip_chain(width, height, rgba.into_raw());
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
/// Both halves matter. **Mipmaps:** a globe tile is [`GLOBE_TILE_PX`]² and an
/// ancestor of it covers far less than that on screen — sampling mip 0 there means
/// near-random access across the whole working set every frame *and* severe
/// minification shimmer. **Off-thread:** the rasterise used to run on the main
/// thread, up to `MAX_GLOBE_TILE_BUILDS_PER_FRAME` times per frame, right in the
/// render loop.
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
    /// The tile server has no such tile (404). An answer, not a failure: the
    /// planet's bottom row is absent from our OSM tiles (`3/0/7` … `3/7/7` all
    /// answer 404), and retrying those every [`QT_RETRY_AFTER_SECS`] kept the globe busy — and a
    /// fresh map load in the load log — for as long as the app ran. The parent
    /// tile stays drawn over the gap.
    Absent,
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
    let center = Vec3::new(
        center_enu.x as f32,
        center_enu.z as f32,
        -center_enu.y as f32,
    );
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
        GLOBE_MESH_USAGE,
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

/// How far a sphere of `radius_m` falls away from its own tangent plane,
/// `horizontal_m` out from the point of tangency.
///
/// The exact sagitta, `R − √(R² − d²)`, rather than the schoolbook `d²/2R`: the
/// approximation is fine at city scale but is 8 m short at the 400 km horizon a
/// 12 km camera sees, and the horizon is precisely where this is read. Clamped
/// past the limb (`d > R`) so a nonsense distance yields the pole rather than a
/// NaN that would poison a whole vertex buffer.
#[must_use]
pub fn curvature_drop_m(horizontal_m: f32, radius_m: f32) -> f32 {
    if !(horizontal_m.is_finite() && radius_m > 0.0) {
        return 0.0;
    }
    let d = horizontal_m.abs().min(radius_m);
    let r = radius_m as f64;
    (r - ((r * r - d as f64 * d as f64).max(0.0)).sqrt()) as f32
}

/// Largest sag a single ground quad may hide, in metres — the knob that turns
/// [`curvature_drop_m`] into a subdivision count.
///
/// A flat quad spanning `s` metres of a sphere misses it by `(s/2)²/2R` in the
/// middle, and the vertices carry the exact drop, so the error is bounded by
/// this and nothing else. 0.25 m is a quarter of the clipmap's 0.4 m level
/// spacing, which is the only other vertical scale in the basemap: any coarser
/// and a level's curvature error could exceed its separation from the level
/// beneath it and the pyramid would start poking through itself on the ring.
const MAX_CURVE_SAG_M: f32 = 0.25;

/// Subdivisions per side a ground tile of `tile_m` needs to follow the curve
/// within [`MAX_CURVE_SAG_M`].
///
/// Deliberately independent of how far out the tile is. The drop grows
/// quadratically with distance from the anchor, but within one tile that growth
/// is a *tilt*, which a flat quad's own corners represent exactly; only the
/// second-order part — the tile's own sag, a function of its size alone — needs
/// vertices. So a z10 ring 80 km out costs the same 7×7 grid as one overhead,
/// and the finest levels (under ~3.5 km) need no subdivision at all.
#[must_use]
pub fn curvature_divisions(tile_m: f32, radius_m: f32) -> usize {
    if !(tile_m > 0.0) || radius_m <= 0.0 {
        return 1;
    }
    // s ≤ 2·√(2·R·sag) is the span whose mid-quad sag stays under the limit.
    let max_span = 2.0 * (2.0 * radius_m * MAX_CURVE_SAG_M).sqrt();
    ((tile_m / max_span).ceil() as usize).clamp(1, 64)
}

/// Altitude (m) at which the space tier begins: the local ground props are
/// hidden, the lighting and atmosphere switch to their orbital models, and
/// `space_factor` starts its ramp.
///
/// It is **no longer** the altitude the globe becomes visible at — see
/// [`GLOBE_DRAW_ALT`]. It used to be both, on the reasoning that "below it the
/// detailed flat-tile city view fully covers the sky-facing hemisphere". That
/// was true at 2 km and false by 10: the flat clipmap reaches ~82 km, a 12 km
/// camera's horizon is ~390 km, and the wedge between them was filled by a
/// 150 km paper disc with a hard rim. Splitting the two constants is what lets
/// the globe carry the far field while the flat tier still owns the near one.
pub const GLOBE_SHOW_ALT: f32 = 50_000.0;

/// Altitude (m) above which the globe is drawn *behind* the flat tier.
///
/// Low enough that the horizon is the planet's at every altitude anyone flies
/// at, high enough that a camera in the street is not paying for a planet it
/// cannot see past the buildings. Between here and [`GLOBE_SHOW_ALT`] both
/// surfaces are on screen at once: the streamed clipmap near the anchor and the
/// quadtree past it, meeting without a step because `GlobeState::drop_at` puts
/// the clipmap on the same sphere.
pub const GLOBE_DRAW_ALT: f32 = 2_000.0;

/// WGS84's semi-major axis, for the handful of places that need *an* Earth
/// radius without a live [`GlobeState`] to read the anchor's real one from —
/// tests, and the ground skirt, which is built before an anchor exists. The
/// geocentric radius runs 6,357–6,378 km, so anything using this is accepting
/// 0.3 %.
pub const GLOBE_EQUATORIAL_RADIUS_M: f32 = 6_378_137.0;

/// The radius the viewer-centred sky shells are authored at (`sky::STAR_SHELL_M`
/// and the moon's own distance), and therefore the floor
/// [`GlobeState::sky_shell_radius`] may never push them below — moving a shell
/// *inward* would change nothing about what it occludes and would shrink the
/// stars.
const SKY_SHELL_GROUND_RADIUS: f32 = 150_000.0;

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
/// stays on: the planet gets its atmospheric limb back. `CARTO_NO_SPACE_RAYMARCH=1`
/// forces the cheap `LookupTexture` path if a weaker GPU ever needs it.
const SPACE_ATMOSPHERE: bool = true;

/// Whether the space tier should raymarch (see [`SPACE_ATMOSPHERE`]),
/// the tier's answer read live.
///
/// Three gates, and the middle one is the load-bearing addition: "a weaker GPU
/// ever needs it" describes a handset exactly, and `CARTO_NO_SPACE_RAYMARCH`
/// cannot be set on one. Android has no process environment and no query string,
/// so every env escape hatch in this codebase is unreachable on the single
/// platform it would matter most for — hence the tier answers first
/// (`quality::Budget::space_raymarch`), and the env flag stays as the way to
/// reproduce the cheap path on a machine that can profile it.
///
/// The switch is read once; the tier's half is read every call, because a preset
/// picked in Settings swaps the budget while the app runs.
fn space_raymarch() -> bool {
    static FORCED_OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    SPACE_ATMOSPHERE
        && crate::quality::budget().space_raymarch
        && !*FORCED_OFF.get_or_init(|| crate::env_flag("CARTO_NO_SPACE_RAYMARCH"))
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
/// ramps up toward [`SPACE_ATMOSPHERE_DENSITY`] in deep space, where the fuller
/// column is what makes the limb glow. See `update_globe_visibility`.
const ATMOSPHERE_DENSITY_NEAR: f32 = 0.12;

/// Atmosphere density multiplier in deep space (`space_factor` 1).
///
/// It was `1 / SPACE_ATMOSPHERE_THICKNESS` — 0.4, the one Earth column spread
/// over the taller shell — and that washed the disc out: measured on the Deck
/// from 14,000 km with realistic land tones, the in-scattered blue outweighed
/// the ground, the Sahara drew grey-lavender and central Africa blue-grey. At
/// 0.15 the same frame draws the Sahara tan (166,150,129), the Congo green and
/// the sea dark blue, and the limb still glows, because the limb's path through
/// the 2.5× shell is long enough that it needs little density to read. 0.06 was
/// tried too and flattens the disc into a map with no air over it.
const SPACE_ATMOSPHERE_DENSITY: f32 = 0.15;

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
    camera: Query<&GridCell, With<crate::components::FreeCamera>>,
    mut planet: Query<(&mut Transform, &mut bevy::light::Atmosphere), With<PlanetAtmosphere>>,
) {
    if !globe.ready {
        return;
    }
    let (Ok(cell), Ok((mut tf, mut atmosphere))) = (camera.single(), planet.single_mut()) else {
        return;
    };
    let edge = crate::grid::WORLD_CELL_EDGE as f64;
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
    //
    // The match makes `r - inner_radius` the camera's altitude in metres, and
    // bevy's sky-view parameterization squares both before subtracting: at 1 m
    // up, `r * r - inner_radius * inner_radius` is a few f32 ulps from zero. The
    // patched `functions.wgsl` (`atmosphere_patch`) guards that `sqrt` against a
    // negative, as bevy's own sibling expression does. From 2026-08-19 Android
    // stepped around it by keeping bevy's 6,360 km instead, at the price of the
    // ring above; the handset's black sky it was blamed for turned out to be
    // something else (`docs/notes/adreno-atmosphere-struct-copy.md`), so every
    // platform matches the radii again.
    if atmosphere.inner_radius != globe.radius {
        atmosphere.inner_radius = globe.radius;
    }
}

/// Show the globe only once high enough that it's the thing you're looking at, so
/// normal ground play never pays for it or sees it poke through the tiles. At the
/// same altitude the atmosphere is disabled (see below) so the planet renders lit
/// against black space instead of washing out or hanging the GPU.
///
/// **`AtmosphereSettings` is optional, and that is the whole point.** This system
/// is the only writer of the globe surface's visibility and of the `HideInSpace`
/// props, but it started life as the atmosphere-mode switch and kept asking for
/// the camera's `AtmosphereSettings` by value in its query. `setup_camera` only
/// inserts that component when the run gets bevy's real atmosphere, so on every
/// run that takes `sky_dome::SkyMode::Dome` instead — the phone tier, an adapter
/// with no `FLOAT32_FILTERABLE` (the Adreno 619), `CARTO_SKY_DOME=1` — the query
/// matched nothing and the system returned before touching a single `Visibility`.
/// The globe then streamed its tiles, built its meshes and never showed any of
/// them: from orbit the handset rendered the 150 km ground disc as a flat grey
/// circle over the anchor, with the globe cloud shell floating above a planet
/// that was not there. `setup_camera` used to note that this system "no-ops
/// without it, which is what made dropping it a safe web workaround" — that was
/// true when the atmosphere was all it did, and silently stopped being true.
#[allow(clippy::type_complexity)]
pub fn update_globe_visibility(
    globe: Res<GlobeState>,
    mut camera: Query<
        (&GridCell, &Transform, Option<&mut AtmosphereSettings>),
        With<crate::components::FreeCamera>,
    >,
    mut planet: Query<&mut bevy::light::Atmosphere, With<PlanetAtmosphere>>,
    media: Option<Res<AtmosphereMedia>>,
    mut scattering_media: ResMut<Assets<bevy::light::atmosphere::ScatteringMedium>>,
    // Density multiplier the current `media.space` asset was built for.
    mut applied_density: Local<f32>,
    mut globe_vis: Query<&mut Visibility, (With<GlobeMesh>, Without<HideInSpace>)>,
    // Only the ground props NOT owned by the Layers toggles (i.e. the horizon
    // disc): tiles, buildings and windows are excluded because
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
            Without<crate::components::OsmBuildingMesh>,
            Without<crate::systems::osm_buildings::WindowGlow>,
            // Trees are owned by `vegetation::update_vegetation_lod`, which hides
            // them at the top of its altitude ramp — far below the space tier, so
            // they are already gone by the time this system would care. Two
            // visibility writers on one entity is the documented race.
            Without<crate::components::VegetationSurface>,
            Without<crate::components::GroundSkirt>,
        ),
    >,
    // The ground skirt, on its own rule (see below).
    mut skirt_vis: Query<
        &mut Visibility,
        (With<crate::components::GroundSkirt>, Without<GlobeMesh>),
    >,
) {
    if !globe.ready {
        return;
    }
    let Ok((cell, tf, atmosphere)) = camera.single_mut() else {
        return;
    };
    // Two different questions, and they used to share one answer. `show_globe`
    // is "draw the planet" and starts at `GLOBE_DRAW_ALT`; `in_space` is "the
    // local scene is over" and still starts at `GLOBE_SHOW_ALT`. Everything
    // below that touches lighting, the atmosphere or the ground props keeps
    // reading `in_space` — only the geometry's own visibility moved.
    let show_globe = globe.drawn(cell, tf);
    let in_space = globe.in_space(cell, tf);

    // Tiered atmosphere *render mode* — the ONLY atmosphere change that's safe at
    // runtime. Near the ground the cheap `LookupTexture` mode suits the city view;
    // from orbit only `Raymarched` renders a planet seen from outside the
    // atmosphere, so switch modes at the globe threshold with a reduced sample
    // count for weak GPUs. Correct only because `update_atmosphere_origin` feeds
    // bevy_pbr the true geocentric camera position (without that, Raymarched from
    // orbit integrated a surface sky dome over the whole screen).
    //
    // **The switch is at `GLOBE_DRAW_ALT`, not `GLOBE_SHOW_ALT`, and that is a
    // correctness fix rather than a matched pair of thresholds.** In
    // `LookupTexture` mode `render_sky.wgsl` composites a geometry pixel as
    // `scene × transmittance(t) + inscattering(t)`, where the transmittance uses
    // the true distance while the in-scattering is clamped at
    // `aerial_view_lut_max_distance` (32 km). Ground past that is therefore
    // extincted without the airlight that should replace it — and since blue
    // scatters out first, what is left is red. That is what turned the old
    // 150 km ground skirt brown, and once the globe carries the view to a 391 km
    // horizon it is 300 km of real map wearing a magenta cast instead.
    //
    // Raymarched takes both terms from the same integral over the true distance
    // with no clamp, which is the whole fix; the far ground washes toward the sky
    // the way aerial perspective actually does. Raising the LUT distance instead
    // was tried and rejected: it stretches the same 32 slices, so buying the
    // horizon costs the near field its accuracy.
    //
    // The weak-GPU escape is unchanged and now covers more ground: a handset that
    // fails `space_raymarch` keeps `LookupTexture` and the magenta with it, which
    // is exactly what it renders today. The other handset path — no
    // `FLOAT32_FILTERABLE`, so `SkyMode::Dome` — has no bevy atmosphere at all and
    // never had the artefact.
    //
    // We deliberately do NOT add/remove the `Atmosphere` or the camera's
    // `AtmosphereEnvironmentMapLight` here (an earlier version dropped the env-map
    // light in space, and hung the device — `VK_ERROR_DEVICE_LOST`, kernel `GPU
    // HANG` logs). Removing them leaves bevy's per-view atmosphere state behind in
    // the render world; the one place that does it, for the Settings switches,
    // is `render_features::apply_sky`, which re-syncs the camera afterwards to
    // clear that state. Nothing here should grow a second.
    //
    // Absent on a `SkyMode::Dome` run, which has no atmosphere to switch modes on
    // — that arm skips this block and goes on to the visibility writes below,
    // which are the part every tier needs.
    if let Some(mut atmosphere) = atmosphere {
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
        let density =
            (1.0f32.lerp(ATMOSPHERE_DENSITY_NEAR, city_fade)).lerp(SPACE_ATMOSPHERE_DENSITY, sf);
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

    let want_globe = if show_globe {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut v in globe_vis.iter_mut() {
        if *v != want_globe {
            *v = want_globe;
        }
    }
    // Hide the skybox and the local ground props once the local scene is over
    // (so the sky reads as black space and no bright patch floats over the
    // anchor); show them again on the way back down. `in_space`, not
    // `show_globe`: these are still the right thing to draw for the whole band
    // where the globe is only filling in past the clipmap.
    let want_local = if in_space {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    for mut v in hide_vis.iter_mut() {
        if *v != want_local {
            *v = want_local;
        }
    }

    // The ground skirt is the one prop that goes at the *globe's* threshold
    // instead, because the globe does not merely outshine it — it replaces it.
    // Both are the surface past the last streamed tile, so leaving the skirt up
    // would put a 150 km cap over the planet it was standing in for, and its rim
    // would cut a straight line across a curved horizon.
    let want_skirt = if show_globe {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    for mut v in skirt_vis.iter_mut() {
        if *v != want_skirt {
            *v = want_skirt;
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
    camera: crate::grid::AnchoredCamera,
    mut albedo: ResMut<GlobeSurfaceAlbedo>,
    mut gloss: ResMut<GlobeOceanGloss>,
    tinted: Query<(&GlobeSurfaceTint, &MeshMaterial3d<GlobeSurfaceMaterial>), With<GlobeMesh>>,
    mut materials: ResMut<Assets<GlobeSurfaceMaterial>>,
    mut applied: Local<Option<f32>>,
    // How far through the live tile set the current step has got. The rewrite is
    // spread over frames (see below), so it has to remember where it was.
    mut cursor: Local<usize>,
) {
    let Some((cell, tf)) = camera.get() else {
        return;
    };
    let space = globe.space_factor(cell, tf);
    let a = 1.0f32.lerp(GLOBE_DEEP_ALBEDO, space);
    albedo.0 = a;
    gloss.0 = space;
    // A step begins when the ramp has drifted far enough, and *ends* only when the
    // cursor has walked the whole set — so the two conditions are separate.
    if *cursor == 0 && applied.is_some_and(|p| (a - p).abs() < ALBEDO_UPDATE_STEP) {
        return;
    }
    // Amortised, because the alternative is a burst at the worst possible moment.
    // A step touches every live tile material — ~450 of them at low orbit, each
    // write dirtying a uniform for re-upload — and it fires during exactly the
    // climbs when the tile build and despawn queues are already at their own
    // per-frame caps. Those two have been budgeted since the device-loss work;
    // this was the third burst in the same frames and the only one without a cap.
    //
    // A tile a few frames behind the ramp is invisible: `ALBEDO_UPDATE_STEP` is
    // 0.03 of a range that takes a whole climb to traverse, and new tiles read the
    // current `GlobeSurfaceAlbedo` at spawn rather than waiting for a sweep.
    let budget = crate::quality::budget().per_frame(MAX_GLOBE_ALBEDO_WRITES_PER_FRAME);
    let mut done = 0;
    for (tint, mat) in tinted.iter().skip(*cursor) {
        if done >= budget {
            *cursor += done;
            return;
        }
        done += 1;
        if let Some(mut m) = materials.get_mut(&mat.0) {
            m.base.base_color = Color::srgb(tint.0.x * a, tint.0.y * a, tint.0.z * a);
            // Only the masked surfaces have an ocean at all; leave the backstop
            // sphere's inert extension alone.
            if m.extension.ocean.y > 0.0 {
                m.extension.ocean.x = space;
            }
        }
    }
    // The sweep completed: bank the value that was being applied, so the next
    // drift past `ALBEDO_UPDATE_STEP` starts a fresh one.
    *cursor = 0;
    *applied = Some(a);
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
                // Report it, rather than returning bare. Every path out of a
                // dispatched job owes the stream a result: `CellStream` releases the
                // in-flight slot when one arrives, so a silent return keys the cell
                // as permanently in flight and it can never be requested again.
                //
                // Today that is harmless, because the only writer of `live_gen`
                // (`setup_globe`) calls `tiles.reset()` a few lines earlier and reset
                // clears `in_flight` wholesale. But that makes the correctness of
                // this function depend on the statement order of another one, with
                // nothing to say so — and `platform::stream` carries a regression
                // test for exactly this failure mode on a different path. Pushing
                // into the old sink is free (reset swapped in a fresh one, so it
                // goes nowhere); pushing into a live one releases the slot.
                if let Ok(mut g) = results.lock() {
                    g.push((key, TileFetch::Cancelled));
                }
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
                    // A tile drawn on an earlier visit comes back as a PNG instead
                    // of being drawn again (`tile_loader::globe_cache_key`).
                    let cache_key =
                        crate::systems::tile_loader::globe_cache_key(&key, &payload, GLOBE_TILE_PX);
                    let stored = match &cache_key {
                        Some(k) => {
                            crate::platform::storage::get(
                                crate::platform::storage::Namespace::Cache,
                                k,
                            )
                            .await
                        }
                        None => None,
                    };
                    // Far: a globe tile is never more urgent than the map under
                    // the camera, and ageing keeps it from starving.
                    crate::platform::workers::submit_ranked(
                        "globe",
                        crate::platform::workers::FAR,
                        Box::new(move || {
                            let reused = stored.and_then(|png| {
                                crate::systems::tile_loader::decode_rendered(&png, GLOBE_TILE_PX)
                            });
                            if let Some(rgba) = reused {
                                if let Ok(mut g) = results.lock() {
                                    g.push((key, TileFetch::Ok(decode_globe_tile(rgba, true))));
                                }
                                return;
                            }
                            // Log the reason, the way the flat streamer's twin does
                            // (`tile_loader::spawn_tile_task`). Discarding it left the
                            // aggregate `globe tile fetches failed` warning below as the
                            // only evidence — a count that reads as a rate limit whether
                            // the tile 404'd, arrived truncated or failed to rasterise.
                            let outcome = match payload.decode(GLOBE_TILE_PX) {
                                Ok((rgba, masked)) => {
                                    if let (true, Some(k)) = (masked, &cache_key) {
                                        crate::systems::tile_loader::store_rendered(k, &rgba);
                                    }
                                    TileFetch::Ok(decode_globe_tile(rgba, masked))
                                }
                                Err(e) => {
                                    tracing::error!(
                                        tile = %key.to_path(),
                                        error = %e,
                                        "globe tile decode failed"
                                    );
                                    TileFetch::Failed
                                }
                            };
                            if let Ok(mut g) = results.lock() {
                                g.push((key, outcome));
                            }
                        }),
                    );
                }
                Err(e) if crate::platform::http::is_absent(&e) => {
                    tracing::debug!(tile = %key.to_path(), "globe tile not served");
                    if let Ok(mut g) = results.lock() {
                        g.push((key, TileFetch::Absent));
                    }
                }
                Err(e) => {
                    tracing::error!(tile = %key.to_path(), error = %e, "globe tile fetch failed");
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
    camera: Query<(&GridCell, &Transform, &Projection), With<crate::components::FreeCamera>>,
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
            // Kept as requested, so it is not asked for again while it stays in
            // view; leaving the view forgets it like any other tile.
            TileFetch::Absent => {
                globe.tiles.adopt(key);
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
        // Byte-bounded through the shared `stream_ledger` too — the imagery mip
        // chain is the cost that matters here; the sphere-patch mesh built below
        // is a rounding error beside it.
        if built >= crate::quality::budget().per_frame(MAX_GLOBE_TILE_BUILDS_PER_FRAME)
            || !crate::systems::stream_ledger::admit(tile.data.len())
        {
            deferred.push((key, TileFetch::Ok(tile)));
            continue;
        }
        globe.tiles.succeeded(&key);
        let has_ocean_mask = tile.has_ocean_mask;
        // Decode + mips already done on the worker; just assemble and upload.
        let image = crate::systems::tile_loader::image_from_mip_chain(
            tile.width,
            tile.height,
            tile.data,
            tile.mip_count,
            TextureFormat::Rgba8UnormSrgb,
            crate::systems::tile_loader::TILE_TEXTURE_USAGE,
            crate::systems::tile_loader::ground_sampler(),
        );
        let texture = images.add(image);

        // Sphere-patch mesh over the tile's Mercator footprint: rows linear in
        // tile-y (so the texture maps 1:1), vertices relative to the tile
        // centre for f32 precision, lifted by zoom so finer tiles sit above.
        let lift = (key.z as f32 * QT_LIFT_PER_Z) as f64 - QT_SURFACE_SINK;
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
        // Tessellation follows the tile's own angular span (see `qt_segs`) — a
        // constant grid leaves a z2 tile as a handful of 830 km flat panels.
        let segs = qt_segs(key.z);
        for iy in 0..=segs {
            let v = iy as f64 / segs as f64;
            let lat = if key.y == 0 && iy == 0 {
                90.0
            } else if key.y == rows - 1 && iy == segs {
                -90.0
            } else {
                tile_frac_to_lat(key.y as f64 + v, key.z)
            };
            for ix in 0..=segs {
                let u = ix as f64 / segs as f64;
                let lng = tile_frac_to_lng(key.x as f64 + u, key.z);
                let p = geodetic_to_local(&frame, lat, lng, lift);
                let rel = p - origin;
                positions.push([rel.x, rel.y, rel.z]);
                let n = (p - globe.center_local).normalize_or_zero();
                normals.push([n.x, n.y, n.z]);
                uvs.push([u as f32, v as f32]);
            }
        }
        let stride = (segs + 1) as u32;
        for iy in 0..segs as u32 {
            for ix in 0..segs as u32 {
                let a = iy * stride + ix;
                let b = a + stride;
                // Outward-front winding (x = east, rows march south).
                indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        let mut mesh = Mesh::new(
            bevy::render::mesh::PrimitiveTopology::TriangleList,
            GLOBE_MESH_USAGE,
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
                        // One rung per zoom level, so a fine tile always wins the
                        // depth test against the coarse one backstopping it — in
                        // depth-buffer units, which is the only currency that holds
                        // at every altitude in the tier (see QT_DEPTH_BIAS_PER_Z).
                        depth_bias: (key.z - QT_MIN_Z) as f32 * QT_DEPTH_BIAS_PER_Z,
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
    let cam = crate::grid::anchored_position_f64(cell, tf.translation);
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

    // Below the space tier the flat clipmap is still drawn and still opaque, so
    // everything within its reach is already painted at a zoom this quadtree
    // cannot match. Tiles that fall wholly inside it are kept but never split:
    // the globe's job down here is the ground *past* the clipmap, and refining
    // what is behind it was the whole cost of drawing the planet from 2 km.
    let covered_to = (altitude <= GLOBE_SHOW_ALT as f64)
        .then(|| crate::systems::map_stream::clipmap_reach_m(globe.anchor_lat));

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
        let view_angle = ((centre - cam).normalize_or_zero().dot(fwd))
            .clamp(-1.0, 1.0)
            .acos();
        let ang_radius = ((gs * 0.75) / d).atan();
        let in_view = view_angle < view_cone + ang_radius;
        // Anchored-frame horizontal distance from the map origin to the tile's
        // near side. `centre` is already in that frame, and the anchor is its
        // origin, so this is just the planar length — exact enough at clipmap
        // range, where the drop is under a kilometre.
        let hidden_by_clipmap = covered_to.is_some_and(|reach| {
            let horizontal = DVec3::new(centre.x, 0.0, centre.z).length();
            horizontal + gs * 0.75 < reach
        });
        if px > QT_SPLIT_PX
            && key.z < QT_MAX_Z
            && in_view
            && !hidden_by_clipmap
            && desired.len() + stack.len() < crate::quality::budget().per_frame(QT_MAX_TILES)
        {
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                stack.push(TileKey::new(key.x * 2 + dx, key.y * 2 + dy, key.z + 1));
            }
        } else {
            desired.push(key);
        }
    }

    // Expand each *not-yet-loaded* leaf with its full ancestor chain (leaf →
    // parent → … → z2).
    // Rendered stacked, coarsest first, so while an uncached fine tile loads the
    // view degrades one level at a time (z10 → z9 → … → z6) instead of jumping
    // straight to the coarse floor. The mid-zoom ancestors (z5–z8) are cheap and
    // usually already on disk, and being coarser they load FIRST under the
    // coarse-first queue — so a sharp-ish fallback appears almost immediately
    // even when the leaf level is uncached and the tile server is throttling.
    // This is the fix for the "fuzzy at 120–170 km" band: there the leaf zoom is
    // ~z9–z10 (partly uncached), and previously the only fallback was the very
    // blurry z3 floor.
    let mut desired_set: HashSet<TileKey> = desired.iter().cloned().collect();
    // …but only while the leaf under them has yet to arrive. The chain is a
    // *loading* fallback, and it used to be unconditional — so every ancestor
    // stayed desired, and the stale-despawn filter below (which only retires what
    // has left `desired`) never reclaimed one. The pyramid was permanent: at low
    // orbit ~170 of ~460 live tiles were coarse levels completely hidden under
    // their own descendants, each still holding a texture, a mesh, a material and
    // a draw call, and each still one more rung of coplanar surface for the depth
    // buffer to separate. Dropping a chain the moment its leaf lands costs
    // nothing — a leaf that later goes missing puts its ancestors straight back in
    // this set, and their bodies are on disk.
    let mut backstops: HashSet<TileKey> = HashSet::new();
    for leaf in &desired {
        if globe.loaded.contains_key(leaf) {
            continue;
        }
        let mut k = leaf.clone();
        while k.z > QT_MIN_Z {
            k = TileKey::new(k.x >> 1, k.y >> 1, k.z - 1);
            if !backstops.insert(k.clone()) {
                break; // this ancestor — and hence all of its own — already added
            }
        }
    }
    desired_set.extend(backstops);
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

    /// The drop is the sagitta, and the schoolbook `d²/2R` is only its first
    /// term. Pinned at the two ends that matter: a city block, where the two
    /// agree to a hair, and the horizon a mid-altitude camera actually sees,
    /// where they do not.
    #[test]
    fn the_drop_is_the_sagitta_not_the_parabola() {
        let r = R as f32;
        for d in [100.0_f32, 1_000.0, 10_000.0] {
            let parabola = d * d / (2.0 * r);
            assert!(
                (curvature_drop_m(d, r) - parabola).abs() < 0.01,
                "{d} m: {} vs {parabola}",
                curvature_drop_m(d, r)
            );
        }
        // 400 km out the parabola is ~8 m short, and 400 km is where the
        // horizon is from 12 km up — i.e. exactly where this gets read.
        let far = curvature_drop_m(400_000.0, r);
        assert!(
            far - 400_000.0f32.powi(2) / (2.0 * r) > 5.0,
            "the two should have visibly parted by 400 km, got {far}"
        );
        // Known values, so a sign flip or a unit slip cannot pass.
        assert!((curvature_drop_m(82_000.0, r) - 527.0).abs() < 2.0);
        assert!((curvature_drop_m(150_000.0, r) - 1_764.0).abs() < 5.0);
    }

    /// Nothing may hand a NaN to a vertex buffer: one poisons every triangle
    /// that touches it, and the tile just vanishes with nothing in the log.
    #[test]
    fn the_drop_is_finite_for_any_input() {
        let r = R as f32;
        for d in [0.0, -5_000.0, f32::MAX, r * 2.0, f32::INFINITY, f32::NAN] {
            assert!(curvature_drop_m(d, r).is_finite(), "{d}");
        }
        assert_eq!(curvature_drop_m(1_000.0, 0.0), 0.0);
    }

    /// The subdivision rule is about the tile's own size and nothing else — a
    /// ring 80 km out costs exactly what the same tile costs overhead. If this
    /// ever became distance-dependent the coarse ring would quietly get 50×
    /// the geometry.
    #[test]
    fn curvature_subdivision_follows_tile_size_alone() {
        let r = R as f32;
        // The finest clipmap levels are flat to well under the limit.
        assert_eq!(curvature_divisions(184.0, r), 1);
        assert_eq!(curvature_divisions(2_900.0, r), 1);
        // …and the coarse rings get a handful of rows, not hundreds.
        let z10 = curvature_divisions(23_500.0, r);
        assert!((4..=12).contains(&z10), "z10 asked for {z10} divisions");
        assert!(curvature_divisions(11_700.0, r) <= z10);
        // Every answer keeps its own sag under the budget it is derived from.
        for tile_m in [184.0_f32, 2_900.0, 11_700.0, 23_500.0, 78_000.0] {
            let span = tile_m / curvature_divisions(tile_m, r) as f32;
            let sag = curvature_drop_m(span * 0.5, r);
            assert!(sag <= MAX_CURVE_SAG_M * 1.001, "{tile_m} m → sag {sag}");
        }
        assert_eq!(curvature_divisions(0.0, r), 1);
    }

    /// The two thresholds are a band, not a pair of names for one number. The
    /// whole point of the split is that there is altitude between them where
    /// both tiers are drawn; collapsing it puts the flat skirt back.
    #[test]
    fn the_globe_is_drawn_well_below_the_space_tier() {
        assert!(GLOBE_DRAW_ALT < GLOBE_SHOW_ALT);
        // Low enough to catch the altitudes the clipmap cannot fill: its reach
        // is ~82 km, and a camera is past that as soon as its horizon is.
        assert!(
            GLOBE_DRAW_ALT <= 5_000.0,
            "the clipmap runs out of ground before {GLOBE_DRAW_ALT} m"
        );
        // The quadtree has to be streaming by the time it is shown, or the
        // globe appears as the bare backstop sphere.
        assert!(QT_MIN_ALT <= GLOBE_DRAW_ALT as f64);
    }

    /// **The load-bearing one: the flat tier's curve is the globe, not a curve
    /// that resembles it.**
    ///
    /// `setup_globe` places every quadtree vertex through `geodetic_to_local`,
    /// and `GlobeState::drop_at` gets the same answer by arithmetic in the flat
    /// frame. If those two ever part company, the seam where the clipmap ends
    /// gains a step — which is the entire defect this exists to remove, so it is
    /// worth a test that does not depend on either side's implementation.
    #[test]
    fn the_flat_tier_curve_lands_on_the_globes_own_surface() {
        let (clat, clng) = (53.2194, 6.5665);
        let frame = TangentFrame::new(clat, clng, 0.0);
        let radius = geodetic_to_local(&frame, clat, clng, 0.0).distance(frame_center(&frame));
        // North and east of the anchor, out to well past the clipmap's reach.
        for metres in [1_000.0_f64, 10_000.0, 82_000.0, 200_000.0] {
            let dlat = metres / 111_320.0;
            let dlng = metres / (111_320.0 * clat.to_radians().cos());
            for p in [
                geodetic_to_local(&frame, clat + dlat, clng, 0.0),
                geodetic_to_local(&frame, clat, clng + dlng, 0.0),
            ] {
                let horizontal = Vec2::new(p.x, p.z).length();
                let ours = curvature_drop_m(horizontal, radius);
                // The globe is an ellipsoid and this is the sphere through the
                // anchor, so they part by the difference between the geocentric
                // radius and the local radii of curvature — 0.4 % of the drop at
                // this latitude, 0.7 % at the equator. What matters is that it
                // stays a fraction of a percent and never becomes a step: at the
                // clipmap's own reach that is two metres against 527 of drop.
                let theirs = -p.y;
                let slack = (theirs.abs() * 0.01).max(0.05);
                assert!(
                    (ours - theirs).abs() < slack,
                    "{metres} m out: flat tier {ours} m vs globe {theirs} m"
                );
            }
        }
    }

    /// Earth's centre in the anchor's local frame — what `setup_globe` computes
    /// as `center_local`.
    fn frame_center(frame: &TangentFrame) -> Vec3 {
        let enu = frame.world_to_local(DVec3::ZERO);
        Vec3::new(enu.x as f32, enu.z as f32, -enu.y as f32)
    }

    /// The quadtree's lift ladder measures outward from the sphere, so the sink
    /// has to swallow the whole of it — otherwise the globe's finest tiles
    /// stand proud of the flat clipmap they are supposed to be filling in
    /// behind, and the planet is drawn over the city.
    #[test]
    fn the_globe_sits_under_the_flat_clipmap() {
        let highest_rung = QT_MAX_Z as f64 * QT_LIFT_PER_Z as f64 - QT_SURFACE_SINK;
        assert!(
            highest_rung < -10.0,
            "the globe's top rung is at {highest_rung} m against a clipmap resting \
             within a few metres of zero"
        );
    }

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

    /// Every zoom's patch must approximate the sphere to within `QT_MAX_SAG_M`.
    ///
    /// This is the "the globe is flat" test. At a fixed 12 segments a z2 tile is
    /// six 830 km panels with 14 km of sag, and a fast zoom-out — which shows the
    /// coarse floor before anything finer arrives — renders the planet as a
    /// faceted plate. Assert the geometry, not the segment count, so the formula
    /// can change as long as the surface it produces still reads as a sphere.
    #[test]
    fn every_zooms_patch_approximates_the_sphere() {
        for z in QT_MIN_Z..=QT_MAX_Z {
            let segs = qt_segs(z);
            let facet = (std::f64::consts::TAU / f64::from(1u32 << z)) / segs as f64;
            let sag = EARTH_RADIUS_M * (1.0 - (facet / 2.0).cos());
            assert!(
                sag <= QT_MAX_SAG_M,
                "z{z}: {segs} segments leaves {sag:.0} m of sag, over the \
                 {QT_MAX_SAG_M:.0} m budget — the globe reads as flat panels"
            );
            assert!(
                segs <= QT_SEGS_MAX,
                "z{z}: {segs} segments is past the mesh ceiling"
            );
        }
    }

    /// The extra tessellation must stay where it is needed. Fine tiles are the
    /// overwhelming majority of the live set, and giving them a z2-sized grid
    /// would multiply the quadtree's vertex count by ~28× for no visible gain.
    #[test]
    fn fine_tiles_keep_the_minimum_grid() {
        for z in 5..=QT_MAX_Z {
            assert_eq!(
                qt_segs(z),
                QT_SEGS,
                "z{z} is paying for coarse-tile curvature"
            );
        }
        assert!(
            qt_segs(QT_MIN_Z) > QT_SEGS,
            "the coarse floor got no refinement"
        );
    }

    /// The ancestor chain that backs a set of leaves — the rule
    /// `update_globe_tiles` applies, lifted out so it can be asserted without a
    /// camera, a `World` or a network.
    fn backstops(leaves: &[TileKey], loaded: &[TileKey]) -> HashSet<TileKey> {
        let mut out: HashSet<TileKey> = HashSet::new();
        for leaf in leaves {
            if loaded.contains(leaf) {
                continue;
            }
            let mut k = leaf.clone();
            while k.z > QT_MIN_Z {
                k = TileKey::new(k.x >> 1, k.y >> 1, k.z - 1);
                if !out.insert(k.clone()) {
                    break;
                }
            }
        }
        out
    }

    /// A leaf that has not arrived is backed by its whole chain, coarsest first —
    /// that is what makes an uncached level degrade one step at a time instead of
    /// dropping to the z3 floor.
    #[test]
    fn a_missing_leaf_is_backed_by_its_whole_chain() {
        let leaf = TileKey::new(1050, 681, 11);
        let chain = backstops(&[leaf.clone()], &[]);
        assert_eq!(chain.len(), (11 - QT_MIN_Z) as usize, "chain has a gap");
        for z in QT_MIN_Z..11 {
            let want = TileKey::new(1050 >> (11 - z), 681 >> (11 - z), z);
            assert!(chain.contains(&want), "z{z} ancestor missing");
        }
    }

    /// …and the moment it arrives, every one of them stops being wanted, which is
    /// what lets the stale filter reclaim them. Before this the chain was
    /// unconditional, so it never left `desired` and the pyramid was permanent:
    /// several hundred fully-occluded coarse tiles holding a texture, a mesh and a
    /// draw call each, for the whole time the view stayed put.
    #[test]
    fn an_arrived_leaf_needs_no_ancestors() {
        let leaf = TileKey::new(1050, 681, 11);
        assert!(backstops(&[leaf.clone()], &[leaf]).is_empty());
    }

    /// A chain is only dropped for the leaf that arrived. Its siblings keep theirs
    /// — including the ancestors they share with it, or a half-loaded quad would
    /// punch a hole through to the backstop sphere.
    #[test]
    fn a_sibling_still_holds_the_shared_chain() {
        let here = TileKey::new(1050, 681, 11);
        let next_door = TileKey::new(1051, 681, 11);
        let chain = backstops(&[here.clone(), next_door], &[here]);
        // They share everything from z10 up (1050>>1 == 1051>>1 == 525).
        assert!(
            chain.contains(&TileKey::new(525, 340, 10)),
            "shared parent lost"
        );
        assert_eq!(chain.len(), (11 - QT_MIN_Z) as usize);
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
