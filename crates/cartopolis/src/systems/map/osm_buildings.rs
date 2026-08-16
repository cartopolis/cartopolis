//! OSM 3D buildings (ADR-020/022) — a camera-following building patch.
//!
//! Fetches OSM building footprints around the camera from the Overpass API and
//! extrudes each footprint to its `height` / `building:levels` tag — both on
//! background threads, so the main thread only turns prepared vertex buffers
//! into assets/entities. Two layers:
//!
//! - **Detail patch** (windows, roof shapes) around the camera, re-fetched and
//!   swapped as the camera drifts.
//! - **Far LOD cells**: simplified extrusions plus a sparse lit-window mesh,
//!   streamed per CACHE_ZOOM cell out to `FAR_CELL_RADIUS` (like map tiles —
//!   fetched once, kept while in range), covering the detail area too so patch
//!   refreshes never flicker; LOD volumes nest inside their detailed twins to
//!   avoid z-fighting.
//!
//! Buildings follow the streamed ground (`map_stream`).
//!
//! Placement maps each vertex through the same Web Mercator math as the tiles
//! (relative to the anchor region tile); heights use the interim quad-to-metre
//! scale until the real-metre frame (ADR-019) lands.

use crate::components::FreeCamera;
use crate::components::OsmBuildingMesh;
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasherDefault;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use web_time::Instant;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::platform::stream::{CellStream, Policy};
use crate::platform::{http, storage};
use cartopolis_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};

/// Fixed grid zoom at which building data is fetched + cached. Independent of the
/// requested patch size, so changing the patch radius never invalidates the cache
/// (each cell is `cache/buildings/{CACHE_ZOOM}/{x}/{y}.json`). Kept coarse (few,
/// large cells) so a patch is a handful of Overpass queries, not dozens.
const CACHE_ZOOM: u32 = 14;
/// Polite pace against a PUBLIC Overpass instance (see [`overpass_pace`]).
const OVERPASS_THROTTLE: Duration = Duration::from_millis(800);
/// Concurrent live queries against our own Overpass (see [`overpass_pace`]).
/// A throughput cap for the VPS (Overpass queries are server-CPU-heavy and the
/// box also runs Nominatim and the tile mirror), not a rate limit.
const OVERPASS_SELF_CONCURRENCY: usize = 4;

/// Overpass endpoint. Defaults to the project's self-hosted instance (seeded with
/// the Netherlands extract, so it doesn't rate-limit under the client's streaming
/// load — see `docs/guides/server.md`). Override with `CARTO_OVERPASS_URL`.
const DEFAULT_OVERPASS_URL: &str = "https://overpass.cartopolis.org/api/interpreter";

/// Public Overpass instance, tried only after the primary endpoint has exhausted
/// its retries. Our own instance being down (or not yet seeded for the area you
/// flew to — it carries a Netherlands extract, and OSM does not stop at the
/// border) otherwise means no buildings at all, which reads as "this city has no
/// buildings" rather than as an outage. The public instance is *not* the default:
/// it is heavily shared and rate-limits under the streaming load a single client
/// generates, so it is a degraded mode, not a peer.
const FALLBACK_OVERPASS_URL: &str = "https://overpass-api.de/api/interpreter";

/// Resolve the primary Overpass endpoint from `CARTO_OVERPASS_URL`, falling back to
/// the self-hosted default.
fn overpass_url() -> String {
    std::env::var("CARTO_OVERPASS_URL").unwrap_or_else(|_| DEFAULT_OVERPASS_URL.to_string())
}

/// Pace for live Overpass queries: `(max concurrent, min spacing between them)`.
///
/// The 1-in-flight / 800 ms budget is citizenship toward a **public** Overpass
/// instance — its rate limits and fairness rules. The default endpoint is our
/// own (`overpass.cartopolis.org`), which has no such budget, and pacing it
/// politely made the far building ring and the POI layers stream several times
/// slower than the server requires. Localhost counts as ours too; any *other*
/// override is assumed public and keeps the polite pace automatically.
pub(crate) fn overpass_pace() -> (usize, Duration) {
    static PACE: OnceLock<(usize, Duration)> = OnceLock::new();
    *PACE.get_or_init(|| {
        let url = overpass_url();
        let ours = url == DEFAULT_OVERPASS_URL
            || url.starts_with("http://localhost")
            || url.starts_with("http://127.");
        if ours {
            (OVERPASS_SELF_CONCURRENCY, Duration::ZERO)
        } else {
            (1, OVERPASS_THROTTLE)
        }
    })
}

/// The endpoints a query walks, in order, with the attempts each one gets.
///
/// The fallback gets fewer attempts than the primary: a cell that reaches it has
/// already spent the primary's full retry budget, and the caller's cooldown is not
/// the only thing that has to stay responsive — every subsequent cell in the patch
/// queues behind this one on the same pool.
fn overpass_endpoints() -> Vec<(String, u32)> {
    let primary = overpass_url();
    let mut out = vec![(primary.clone(), 3)];
    if primary != FALLBACK_OVERPASS_URL {
        out.push((FALLBACK_OVERPASS_URL.to_string(), 2));
    }
    out
}

/// Space the *next* fallback request at least [`OVERPASS_THROTTLE`] after the
/// previous one, across all callers.
///
/// [`overpass_pace`] paces the dispatcher by the **primary** endpoint, so with
/// ours as primary the far ring runs 4-wide with no spacing — correct for our own
/// box, and exactly what must not be pointed at a public instance. Without this
/// gate a primary outage would turn that unpaced fan-out onto `overpass-api.de`
/// wholesale. Spacing (not serialising) is what the throttle has always meant:
/// the lock is released before the request goes out, so a slow answer doesn't
/// stall the one behind it.
#[cfg(not(target_arch = "wasm32"))]
fn pace_fallback() {
    use std::sync::Mutex;
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    // Poisoning only means some other caller panicked mid-pace; the timestamp is
    // still meaningful, and refusing to pace is the worse failure here.
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(prev) = *last {
        let waited = prev.elapsed();
        if waited < OVERPASS_THROTTLE {
            std::thread::sleep(OVERPASS_THROTTLE - waited);
        }
    }
    *last = Some(Instant::now());
}

/// Self-hosted 3DBAG static building host (real Dutch BAG heights + ground level;
/// ADR-027). Serves Overpass-shaped per-cell JSON at
/// `/buildings/{CACHE_ZOOM}/{x}/{y}.json` plus a `/coverage.json` manifest, so
/// covered cells parse through the same `parse_buildings` path as Overpass — only
/// the source differs. Override with `CARTO_3DBAG_URL`; empty disables 3DBAG.
const DEFAULT_3DBAG_URL: &str = "https://3dbag.cartopolis.org";

/// Resolve the 3DBAG host from `CARTO_3DBAG_URL` (empty string disables 3DBAG,
/// falling back to OSM buildings everywhere).
pub(crate) fn tdbag_url() -> String {
    std::env::var("CARTO_3DBAG_URL").unwrap_or_else(|_| DEFAULT_3DBAG_URL.to_string())
}

/// Which fixed-zoom cells the 3DBAG host serves, loaded once from its coverage
/// manifest. Cells outside this set (and any 3DBAG fetch miss) fall through to
/// Overpass/OSM.
struct Coverage {
    zoom: u32,
    cells: HashSet<(i64, i64)>,
    attribution: String,
}

static COVERAGE: OnceLock<Coverage> = OnceLock::new();
static COVERAGE_LOD22: OnceLock<Coverage> = OnceLock::new();
/// A coverage fetch is in flight (see [`ensure_coverage_prefetch`]).
static COVERAGE_FETCHING: AtomicBool = AtomicBool::new(false);
/// When the last coverage fetch was started, as `f64` bits; 0 = never.
static COVERAGE_LAST_ATTEMPT: AtomicU64 = AtomicU64::new(0);
/// Spacing between coverage retries while the 3DBAG host is unreachable. Long
/// enough not to matter — the answer never changes for a running client, and the
/// fallback (Overpass footprints) is already on screen while we wait.
const COVERAGE_RETRY_SECS: f64 = 30.0;
/// The Layers panel's "3DBAG (NL)" toggle, mirrored in by
/// `data_layers::apply_feature_toggles` each frame — the coverage predicates are
/// free functions over statics, out of reach of the `DataLayers` resource. While
/// `false`, [`tdbag_covers`] / [`lod22_covers`] report no coverage, which routes
/// every building path to its existing OSM fallback (the same behaviour as an
/// unreachable 3DBAG host).
static TDBAG_USER_ENABLED: AtomicBool = AtomicBool::new(true);

/// Mirror the Layers-panel 3DBAG toggle into the coverage gate.
pub(crate) fn set_tdbag_enabled(on: bool) {
    TDBAG_USER_ENABLED.store(on, Ordering::Relaxed);
}

/// Current state of the Layers-panel 3DBAG toggle.
pub(crate) fn tdbag_user_enabled() -> bool {
    TDBAG_USER_ENABLED.load(Ordering::Relaxed)
}

/// The Layers-panel **master** Buildings toggle, mirrored the same way and for the
/// same reason as [`TDBAG_USER_ENABLED`] above: both building streamers are at
/// Bevy's 16-parameter ceiling and cannot take `Res<DataLayers>`.
///
/// Until this existed the master toggle reached only `ToggleWants::of`, which
/// writes `Visibility::Hidden` — so unticking Buildings hid the meshes and changed
/// nothing else. The client went on fetching 49 OSM cells and 9 3DBAG cells,
/// decoding and triangulating them on the *same* `AsyncComputeTaskPool` the vector
/// tile rasterise runs on (which CLAUDE.md documents as starvable by exactly this
/// kind of long blocking job), uploading a merged mesh plus four window meshes per
/// cell, spawning a few hundred entities and publishing their collider polygons.
///
/// So the most expensive layer in the client was the one whose "off" did the least,
/// on the device with the least headroom.
static BUILDINGS_USER_ENABLED: AtomicBool = AtomicBool::new(true);

/// Mirror the Layers-panel master Buildings toggle out of `DataLayers`.
pub(crate) fn set_buildings_enabled(on: bool) {
    BUILDINGS_USER_ENABLED.store(on, Ordering::Relaxed);
}

/// Current state of the Layers-panel master Buildings toggle.
pub(crate) fn buildings_user_enabled() -> bool {
    BUILDINGS_USER_ENABLED.load(Ordering::Relaxed)
}

/// Fetch a 3DBAG coverage manifest via the http seam. A missing/unreachable host
/// (or an empty tile URL) yields empty coverage, so the client silently uses OSM
/// buildings everywhere.
/// `None` means *we could not ask* — a transient network failure, which must not
/// be latched. `Some(empty)` is a real answer: no 3DBAG configured, or a host that
/// served something unusable.
///
/// The distinction is the whole point of the return type. Folding an unreachable
/// host into the empty coverage made a single failed request at startup disable
/// 3DBAG buildings for the entire life of the process, silently and
/// unrecoverably — `coverage_resolved()` reported `true`, every `*_covers` call
/// answered `false`, and the client spent the session on Overpass footprints with
/// nothing in the log to say a better source existed.
async fn fetch_coverage(sub: &str) -> Option<Coverage> {
    let empty = Coverage {
        zoom: CACHE_ZOOM,
        cells: HashSet::new(),
        attribution: String::new(),
    };
    let base = tdbag_url();
    if base.is_empty() {
        return Some(empty); // configured off: a definitive "no 3DBAG here"
    }
    let url = format!("{base}{sub}");
    let text = match http::fetch_string(http::Request::get(url.clone())).await {
        Ok(t) => t,
        Err(e) => {
            tracing::info!(%url, error = %e, "3DBAG coverage unreachable; will retry");
            return None;
        }
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        // The host answered, with something we cannot read. Retrying would ask the
        // same question and get the same answer, so this one does latch.
        tracing::warn!(%url, "3DBAG coverage is not JSON; using OSM buildings");
        return Some(empty);
    };
    let zoom = json
        .get("zoom")
        .and_then(|z| z.as_u64())
        .unwrap_or(CACHE_ZOOM as u64) as u32;
    let attribution = json
        .get("attribution")
        .and_then(|a| a.as_str())
        .unwrap_or_default()
        .to_string();
    let mut cells = HashSet::new();
    if let Some(arr) = json.get("cells").and_then(|c| c.as_array()) {
        for cell in arr {
            if let Some(pair) = cell.as_array() {
                if let (Some(x), Some(y)) = (
                    pair.first().and_then(|v| v.as_i64()),
                    pair.get(1).and_then(|v| v.as_i64()),
                ) {
                    cells.insert((x, y));
                }
            }
        }
    }
    tracing::info!(sub, cells = cells.len(), zoom, "3DBAG coverage loaded");
    Some(Coverage {
        zoom,
        cells,
        attribution,
    })
}

/// Fetch both coverage manifests, retrying while the host is unreachable.
///
/// Idempotent and cheap to call every frame. Until they land, [`tdbag_covers`] /
/// [`lod22_covers`] report `false`, so the client falls back to Overpass footprints
/// and far extrusions, then switches to 3DBAG once coverage arrives. This replaces
/// the old blocking `get_or_init`, which stalled whichever fetch first touched it
/// on the network (and could not run on wasm at all).
///
/// It used to be strictly one-shot, which paired badly with treating an
/// unreachable host as "no coverage": one failed request during startup — the
/// moment a mobile client is least likely to have a working connection — latched
/// the empty answer into a `OnceLock` and disabled 3DBAG for the process. Retrying
/// is the other half of `fetch_coverage` returning `Option`.
fn ensure_coverage_prefetch(now: f64) {
    if coverage_resolved() || COVERAGE_FETCHING.load(Ordering::Relaxed) {
        return;
    }
    let last = f64::from_bits(COVERAGE_LAST_ATTEMPT.load(Ordering::Relaxed));
    if last > 0.0 && now - last < COVERAGE_RETRY_SECS {
        return;
    }
    COVERAGE_FETCHING.store(true, Ordering::Relaxed);
    COVERAGE_LAST_ATTEMPT.store(now.to_bits(), Ordering::Relaxed);
    IoTaskPool::get()
        .spawn(async {
            // Each manifest is asked for only while still unanswered, so a run
            // where one succeeded and the other did not costs one request, not two.
            if COVERAGE.get().is_none() {
                if let Some(c) = fetch_coverage("/coverage.json").await {
                    let _ = COVERAGE.set(c);
                }
            }
            if COVERAGE_LOD22.get().is_none() {
                if let Some(c) = fetch_coverage("/lod22/coverage.json").await {
                    let _ = COVERAGE_LOD22.set(c);
                }
            }
            COVERAGE_FETCHING.store(false, Ordering::Relaxed);
        })
        .detach();
}

/// True once both coverage manifests have resolved — successfully or as the
/// empty "no 3DBAG" fallback. Before that, [`tdbag_covers`] / [`lod22_covers`]
/// answer `false` for cells the host actually serves, and building work started
/// on that answer is wasted: the serial Overpass lane filled with covered-cell
/// fetches (800 ms courtesy sleep each) that the real meshes evicted moments
/// later. Resolution is one round-trip to our own host (instant when
/// `CARTO_3DBAG_URL` is empty), so briefly holding building work for it is
/// cheaper than doing it twice.
pub(crate) fn coverage_resolved() -> bool {
    COVERAGE.get().is_some() && COVERAGE_LOD22.get().is_some()
}

/// True if the 3DBAG host serves this cell's LoD1.2 footprints. `false` until the
/// coverage manifest has loaded (see [`ensure_coverage_prefetch`]), and while the
/// Layers panel's 3DBAG toggle is off.
fn tdbag_covers(cx: i64, cy: i64) -> bool {
    tdbag_user_enabled()
        && COVERAGE
            .get()
            .is_some_and(|cov| cov.zoom == CACHE_ZOOM && cov.cells.contains(&(cx, cy)))
}

/// True if the 3DBAG host serves this cell as a real LoD2.2 mesh (rendered by
/// `lod22.rs`). The OSM/LoD1.2 building layers skip these cells so the real meshes
/// render without overlapping simplified extrusions. `false` until coverage loads,
/// and while the Layers panel's 3DBAG toggle is off.
pub(crate) fn lod22_covers(cx: i64, cy: i64) -> bool {
    tdbag_user_enabled()
        && COVERAGE_LOD22
            .get()
            .is_some_and(|cov| cov.zoom == CACHE_ZOOM && cov.cells.contains(&(cx, cy)))
}

/// The 3DBAG attribution string, if any 3DBAG coverage (LoD2.2 or LoD1.2) is
/// loaded and non-empty — for the UI credits (CC BY 4.0). Non-blocking: reads the
/// already-initialised manifests and returns `None` until a building fetch has
/// loaded coverage, so it never triggers the network fetch itself.
pub fn tdbag_attribution() -> Option<String> {
    for cov in [COVERAGE_LOD22.get(), COVERAGE.get()].into_iter().flatten() {
        if !cov.cells.is_empty() && !cov.attribution.is_empty() {
            return Some(cov.attribution.clone());
        }
    }
    None
}

/// Wall + tiled-roof colour for a LoD2.2 building at a location, reusing the same
/// palette/jitter as the extruded buildings so 3DBAG and OSM buildings share a
/// look. (`lod22.rs` picks grey for near-flat roof triangles by slope.)
pub(crate) fn building_palette(lat: f64, lng: f64) -> ([f32; 3], [f32; 3]) {
    let seed = color_seed((lat, lng));
    let wall = jitter(palette_color((lat, lng)), seed, 0.12);
    let roof = jitter(default_roof("yes", seed), seed.rotate_left(17), 0.12);
    (wall, roof)
}

/// Roof form from OSM `roof:shape` (Simple 3D Buildings).
#[derive(Clone, Copy, Debug, PartialEq)]
enum RoofShape {
    Flat,
    Gabled,
    Hipped,
    Pyramidal,
    Skillion,
}

/// One OSM building: OSM way id (for cross-cell dedup + future per-building
/// conversion), footprint in (lat, lng), height in metres, wall/roof colours, and
/// roof form/height derived from its tags (with fallbacks).
#[derive(Clone, Debug)]
struct Building {
    id: i64,
    ring: Vec<(f64, f64)>,
    /// Either total height (from `height`) or eaves/wall height (from levels or an
    /// estimate) — `height_is_eaves` says which, so the roof sits correctly.
    height_m: f32,
    height_is_eaves: bool,
    wall_color: [f32; 3],
    roof_color: [f32; 3],
    roof_shape: RoofShape,
    /// Explicit `roof:height` in metres, if tagged.
    roof_height: Option<f32>,
    /// `roof:orientation=across` (ridge across the long axis instead of along it).
    roof_across: bool,
}

/// A detail patch fully prepared on the background thread — fetched footprints
/// *and* built meshes — so the main thread only creates assets and entities (no
/// frame hitch from extruding thousands of buildings).
struct BuiltPatch {
    /// Footprints, retained for hit-testing / prim conversion.
    buildings: Vec<Building>,
    /// BigSpace cell the mesh vertices are relative to.
    patch_cell: GridCell,
    /// Full-detail buildings around the camera.
    detail: Option<Mesh>,
    /// Window quads per glow bucket (indexes `WINDOW_GLOW_MULT`).
    windows: [Option<Mesh>; 4],
}

/// Camera-following building patch: re-fetched from Overpass when the camera
/// drifts, with the old set despawned (ADR-020/022). Anchored to the region tile
/// so buildings stay aligned with the streamed ground.
#[derive(Resource)]
pub struct OsmBuildings {
    /// Anchor: tile_x, tile_y, zoom, latitude — derived from the map anchor
    /// (`MapStream`), so buildings follow re-anchors to any location (ADR-025).
    anchor: Option<(i64, i64, u32, f64)>,
    /// Terrain-field revision the live buildings stand on; a change rebuilds them.
    terrain_revision: u32,
    /// Last-seen master Buildings toggle, so its off-edge can tear the patch down
    /// once instead of every frame it stays off. Mirrors `Lod22Buildings::
    /// enabled_was`, which does the same job for the 3DBAG sub-toggle.
    enabled_was: bool,
    /// Centre tile of the last fetch (re-fetch when the camera moves away).
    last_center: Option<(i64, i64)>,
    /// Last-seen (3DBAG toggle, coverage resolved) pair. A patch bakes its
    /// building set under one setting (covered cells filtered out, or not), so
    /// either flipping must force a refetch — the drift trigger alone can leave
    /// a stale patch standing under (or holed inside) the real meshes
    /// indefinitely.
    tdbag_was: Option<(bool, bool)>,
    /// When the last detail fetch was kicked off (`DETAIL_REFETCH_COOLDOWN`).
    last_fetch: Option<Instant>,
    in_flight: bool,
    /// Latest completed detail fetch+build, consumed on the main thread.
    fetch: Arc<Mutex<Option<Result<BuiltPatch, String>>>>,
    /// Detail-ring mesh entities (buildings + windows), despawned on re-fetch.
    detail_entities: Vec<Entity>,
    /// Far LOD cell streaming for **Overpass** cells (CACHE_ZOOM cell coords): the
    /// requested/failed bookkeeping, the results sink, the concurrency bound and
    /// the bounded despawn queue (`platform::stream`). Paced by the endpoint —
    /// concurrent against our own Overpass, serial against a public one, where the
    /// spacing comes from the throttle inside the fetch.
    ///
    /// This lane also owns the **shared** despawn queue: a retired far cell is
    /// retired regardless of which lane fetched it.
    #[allow(clippy::type_complexity)]
    far: CellStream<(i64, i64), Result<Option<FarCellBuild>, String>>,
    /// The same, for **3DBAG-covered** cells. Our own static host has no rate
    /// limit, so these run concurrently instead of riding the Overpass courtesy
    /// lane — which made a covered city stream its far ring one cell at a time for
    /// no one's benefit. A separate stream rather than a flag: a cell then lives in
    /// exactly one lane's bookkeeping, so a coverage answer that flips mid-flight
    /// cannot free the wrong lane's slot.
    #[allow(clippy::type_complexity)]
    far_tdbag: CellStream<(i64, i64), Result<Option<FarCellBuild>, String>>,
    /// Spawned far-cell mesh entities (buildings + windows), despawned when out
    /// of range.
    far_cells: HashMap<(i64, i64), Vec<Entity>>,
    /// Shared material for all far-cell building meshes.
    far_material: Option<Handle<StandardMaterial>>,
    /// Per-bucket window materials shared by all far cells; their emissive is
    /// driven by `update_window_glow` through the cells' `WindowGlow` entities.
    far_window_materials: Option<[Handle<StandardMaterial>; 4]>,
    /// Per-building data for the current patch, retained so an individual building
    /// can later be hit-tested and converted to an editable prim (ADR-020/021).
    buildings: Vec<Building>,
}

impl OsmBuildings {
    /// Is the detail patch or either far lane still working? Read by the `--shot`
    /// harness's settle test: the OSM detail patch is the slowest thing on the
    /// map (one Overpass round trip per re-anchor) and it is exactly what a
    /// ground-level capture is waiting for.
    #[must_use]
    pub fn streaming_busy(&self) -> bool {
        self.in_flight || self.far.busy() || self.far_tdbag.busy()
    }

    /// The far lane a cell belongs to. Routed by coverage at request time and
    /// re-derived from the same predicate at completion, which is safe because the
    /// two lanes keep separate bookkeeping — the worst a flip mid-flight can do is
    /// leave one lane holding a slot until the next reset, never free a slot that
    /// another cell owns.
    fn far_lane(
        &mut self,
        tdbag: bool,
    ) -> &mut CellStream<(i64, i64), Result<Option<FarCellBuild>, String>> {
        if tdbag {
            &mut self.far_tdbag
        } else {
            &mut self.far
        }
    }

    /// Despawn the far-cell extrusion for `cell`, if one is up — called by the
    /// LoD2.2 streamer the moment its real mesh spawns there, so the two tiers
    /// never stack. The far tier legitimately requests covered cells whenever
    /// `lod22_covers` says no *at request time* — before the coverage manifest
    /// arrives at startup, or while the Layers panel's 3DBAG toggle is off —
    /// and nothing else removes them: far cells are only pruned by camera
    /// distance, and the inset footprint does not hide them (the extruded
    /// height is a guess that can poke through the real BAG roof). Forgetting the
    /// cell in both lanes lets the far tier re-fetch it (disk-cached) when the real
    /// mesh later goes out of range.
    pub(crate) fn evict_far_cell(&mut self, commands: &mut Commands, cell: (i64, i64)) {
        for e in self.far_cells.remove(&cell).unwrap_or_default() {
            commands.entity(e).despawn();
        }
        self.far.forget(&cell);
        self.far_tdbag.forget(&cell);
    }
}

impl Default for OsmBuildings {
    fn default() -> Self {
        Self {
            anchor: None,
            // True, matching `BUILDINGS_USER_ENABLED`'s own initial value, so a run
            // that never touches the toggle sees no edge and tears nothing down.
            enabled_was: true,
            last_center: None,
            tdbag_was: None,
            last_fetch: None,
            in_flight: false,
            fetch: Arc::default(),
            detail_entities: Vec::new(),
            far: CellStream::new(Policy {
                // Paced by the endpoint (`overpass_pace`): concurrent against our
                // own instance, serial against a public one — which is what a
                // shared endpoint is owed, and where the spacing comes from the
                // throttle inside the fetch.
                max_in_flight: overpass_pace().0,
                min_interval_secs: 0.0,
                max_attempts: FAR_CELL_MAX_ATTEMPTS,
                backoff_secs: FAR_CELL_BACKOFF_SECS,
                backoff_doublings: FAR_CELL_BACKOFF_DOUBLINGS,
                cooldown_secs: FAR_CELL_RETRY_AFTER_SECS,
                // This lane owns the shared despawn queue; the 3DBAG one below
                // never retires anything of its own.
                despawns_per_frame: MAX_FAR_DESPAWNS_PER_FRAME,
            }),
            far_tdbag: CellStream::new(Policy {
                max_in_flight: TDBAG_FAR_CONCURRENCY,
                min_interval_secs: 0.0,
                max_attempts: FAR_CELL_MAX_ATTEMPTS,
                backoff_secs: FAR_CELL_BACKOFF_SECS,
                backoff_doublings: FAR_CELL_BACKOFF_DOUBLINGS,
                cooldown_secs: FAR_CELL_RETRY_AFTER_SECS,
                despawns_per_frame: 0,
            }),
            terrain_revision: 0,
            far_cells: HashMap::new(),
            far_material: None,
            far_window_materials: None,
            buildings: Vec::new(),
        }
    }
}

/// Attempts before a far cell is left alone. Overpass 504s under load, and a cell
/// that has failed three times is either genuinely empty upstream or the query is
/// too heavy for it — a fourth try costs a shared endpoint a request to find out.
const FAR_CELL_MAX_ATTEMPTS: u32 = 3;

/// Wait after a far cell's first failure, doubling [`FAR_CELL_BACKOFF_DOUBLINGS`]
/// times: 2 s, 4 s, 8 s.
///
/// Without it the three attempts above were spent on three consecutive *frames* —
/// 50 ms — which is not three chances at a flaky endpoint, it is one chance
/// repeated thrice into the same outage. The reason to stop at three is to spare a
/// shared endpoint, and a backoff is what actually does that.
const FAR_CELL_BACKOFF_SECS: f64 = 2.0;
const FAR_CELL_BACKOFF_DOUBLINGS: u32 = 2;

/// How long an exhausted far cell rests before its attempt count is forgiven.
///
/// This used to be 0.0, which `platform::stream::Policy` reads as **never** — so
/// three 504s during one bad minute at Overpass left a permanent hole in the
/// buildings for the whole life of the map anchor, healed only by a teleport. The
/// comment on the attempt cap named 504s-under-load as the expected failure and
/// then treated them as if they were "absent upstream". A minute is long enough to
/// be no burden on the endpoint and short enough that a user who looks up from the
/// map sees the block arrive.
const FAR_CELL_RETRY_AFTER_SECS: f64 = 60.0;

/// Far cells' entities freed per frame. A far cell is one merged extrusion mesh
/// plus its window meshes, and a re-anchor retires all `(2·FAR_CELL_RADIUS+1)² =
/// 49` of them at once — the documented `VK_ERROR_DEVICE_LOST` burst. Higher than
/// `lod22`'s cap because these meshes are the simplified tier and far cheaper.
const MAX_FAR_DESPAWNS_PER_FRAME: usize = 6;

/// Far cells *built* per frame — the other half of the guard above, which the
/// despawn side has had all along and the build side did not.
///
/// Two rather than `lod22`'s one, for the same reason the despawn cap is six:
/// these are the simplified tier, one merged extrusion plus a few window meshes,
/// where a LoD2.2 cell is a full triangle soup. `Budget::per_frame` halves it to
/// one on the phone.
const MAX_FAR_BUILDS_PER_FRAME: usize = 2;

/// An emissive window mesh; the value is its brightness multiplier (0 = never lit),
/// scaled by the night factor in `update_window_glow`.
#[derive(Component)]
pub struct WindowGlow(pub f32);

/// Cull back faces on the wall/roof materials (mirrors `lod22::BACKFACE_CULL`).
///
/// Building solids are closed, and every generator now emits consistently outward
/// windings via [`MeshData::tri_facing`] / [`MeshData::quad_facing`], so roughly
/// half the triangles face away and need not be rasterized — in the main view and,
/// for the shadow-casting detail patch, in each cascade too. Set `false` (→
/// double-sided, the old behaviour) if any building renders inside-out.
///
/// Only the wall/roof materials; the window quads stay double-sided since they're
/// genuinely single-sided geometry sitting proud of the facade.
const BACKFACE_CULL: bool = true;

/// The wall/roof `cull_mode`, gated by [`BACKFACE_CULL`].
fn building_cull_mode() -> Option<bevy::render::render_resource::Face> {
    BACKFACE_CULL.then_some(bevy::render::render_resource::Face::Back)
}

/// Default metres per storey when only `building:levels` is present.
const STOREY_HEIGHT: f32 = 3.0;

/// Only buildings with eaves at least this tall get windows (skips houses/sheds).
const WINDOW_MIN_HEIGHT: f32 = 8.0;
/// Per-window brightness buckets: bucket 0 stays dark, the rest glow at these
/// multipliers — so at night some windows are off, some dim, some bright.
const WINDOW_GLOW_MULT: [f32; 4] = [0.0, 0.5, 0.9, 1.35];

/// Window-grid parameters for one LOD tier (all lengths in metres).
struct WindowGrid {
    /// Column pitch along the wall.
    pitch: f32,
    /// Quad width and height.
    w: f32,
    h: f32,
    /// Vertical floor spacing and first-floor sill height.
    floor: f32,
    sill: f32,
    /// Offset proud of the wall (avoids z-fighting the facade).
    proud: f32,
    /// Skip the never-lit bucket entirely (pointless to emit at range).
    skip_dark: bool,
}

/// Full-detail grid: real window sizes on real floor spacing.
const DETAIL_WINDOWS: WindowGrid = WindowGrid {
    pitch: 2.6,
    w: 1.3,
    h: 1.5,
    floor: 3.0,
    sill: 1.2,
    proud: 0.06,
    skip_dark: false,
};

/// Far-LOD grid: sparse, oversized quads — from kilometres away this reads as the
/// same lit-window pattern at roughly a fifth of the vertex count, and real windows
/// would be sub-pixel shimmer at that range anyway. Dark windows are never emitted.
const FAR_WINDOWS: WindowGrid = WindowGrid {
    pitch: 7.0,
    w: 2.8,
    h: 2.2,
    floor: 6.0,
    sill: 1.6,
    proud: 0.15,
    skip_dark: true,
};

/// Tiles out from the camera covered by the full-detail building patch
/// (windows, roof shapes).
const BUILDING_PATCH_RADIUS: i64 = 3;

/// [`BUILDING_PATCH_RADIUS`] with the render budget applied — 2 (5×5) on the phone
/// tier instead of 3 (7×7), i.e. roughly half the area.
///
/// A function rather than a tier-adjusted constant, for the reason
/// [`lod22::lod22_radius`] gives at greater length: the radii in this module are
/// read against each other, and two separately-adjusted constants can drift where
/// a shared call cannot.
///
/// This is the patch that carries real window grids on real floor spacing
/// ([`DETAIL_WINDOWS`]), so its area is close to linear in triangle count — it is
/// the most expensive ring in the client per tile.
fn building_patch_radius() -> i64 {
    crate::quality::budget().radius(BUILDING_PATCH_RADIUS)
}

/// Concurrent far-cell fetches against the 3DBAG host (see
/// `OsmBuildings::far_dispatch_tdbag`). Same reasoning as `LOD22_CONCURRENCY`:
/// our own host, so this is a throughput cap, not a throttle.
const TDBAG_FAR_CONCURRENCY: usize = 4;
/// CACHE_ZOOM cells out from the camera covered by far-LOD building cells
/// (radius 3 → 7×7 cells ≈ ±5 km). Each cell streams in independently (fetched
/// once, meshed on the worker, kept while in range), so this radius costs
/// nothing on camera drift — unlike the old monolithic far patch, which was
/// re-fetched and rebuilt wholesale. Covers the detail area too, as a
/// flicker-free fallback while a detail refresh is in flight.
const FAR_CELL_RADIUS: i64 = 3;

/// [`FAR_CELL_RADIUS`] with the render budget applied — 2 (5×5 = 25 cells) on the
/// phone tier instead of 3 (7×7 = 49).
///
/// Must stay at or beyond `lod22::lod22_radius() + 1`, which is where the real
/// LoD2.2 meshes hand over to these simplified extrusions. Below that there would
/// be a ring of covered cells that neither streamer owns — buildings simply
/// missing, at a distance that changes with the tier. `the_far_ring_meets_the_lod22_
/// handover` pins it on both tiers.
fn far_cell_radius() -> i64 {
    crate::quality::budget().radius(FAR_CELL_RADIUS)
}
/// Re-fetch the patch once the camera has moved this many tiles from the last centre.
/// Larger = fewer Overpass calls (avoids 429/504 rate limits) at the cost of the
/// patch trailing the camera a bit further before refreshing.
const REFETCH_TILES: i64 = 4;
/// Freeze detail-patch re-fetches above this altitude (m). Individual buildings
/// are a few pixels from up here and the far cells carry the look — but a
/// zoom-out sweep drags the fetch centre kilometres sideways, and every drift
/// re-fetch despawns + re-uploads a giant merged patch mesh. One zoom-out fired
/// 13 full rebuilds (6,776 → 46 buildings) in 2.4 s and the burst of uploads
/// overran the iGPU's command-buffer timeout (`VK_ERROR_DEVICE_LOST`) — the same
/// failure shape the flat-tile and globe-tile per-frame build caps guard against.
const DETAIL_FREEZE_ALT: f32 = 3_000.0;
/// Minimum spacing between detail-patch re-fetches (the storm brake for fast
/// low-altitude panning; each rebuild is a multi-MB mesh upload).
const DETAIL_REFETCH_COOLDOWN: Duration = Duration::from_secs(2);

/// Muted, realistic wall tones (tans, greys, beige, muted brick, off-white).
const BUILDING_COLORS: [[f32; 3]; 8] = [
    [0.80, 0.76, 0.70],
    [0.72, 0.70, 0.66],
    [0.78, 0.72, 0.64],
    [0.68, 0.66, 0.64],
    [0.76, 0.63, 0.56],
    [0.83, 0.81, 0.77],
    [0.70, 0.65, 0.58],
    [0.74, 0.71, 0.69],
];

/// Tiled-roof tones for residential buildings without a `roof:*` tag (terracotta,
/// brown-red, dark tile, grey tile) — the characteristic look of a Dutch town.
const ROOF_COLORS: [[f32; 3]; 5] = [
    [0.62, 0.30, 0.24],
    [0.55, 0.34, 0.26],
    [0.45, 0.32, 0.28],
    [0.50, 0.50, 0.52],
    [0.40, 0.28, 0.25],
];

/// Re-fetch and respawn the building patch as the camera roams (ADR-020/022).
pub fn update_buildings(
    mut commands: Commands,
    mut osm: ResMut<OsmBuildings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    stream: Res<crate::systems::map_stream::MapStream>,
    time: Res<Time>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    globe: Res<crate::systems::globe::GlobeState>,
    mut colliders: ResMut<crate::systems::collision::BuildingColliders>,
    terrain: Res<crate::systems::terrain::TerrainField>,
) {
    // Fire the 3DBAG coverage prefetch once (async, off the main thread) — the
    // coverage predicates report "uncovered" until it lands, then upgrade.
    ensure_coverage_prefetch(time.elapsed_secs_f64());

    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };

    // Re-anchor (ADR-025): when the map origin moves, tear down this patch and
    // re-establish it at the new location. Dropping `far_jobs` ends the old
    // worker (its `recv` errors), and a fresh `far_results` orphans any in-flight
    // result it still pushes, so nothing from the old origin leaks through.
    // Free retired far cells at the shared per-frame cap, before any early return.
    osm.far.drain_despawns(&mut commands);

    // The measured ground lands after the anchor it belongs to, so the first
    // buildings are always extruded onto the flat plane; rebuilding them is the
    // same teardown a re-anchor does. `sync` runs unconditionally so it adopts the
    // new generation either way.
    // The Layers panel's master Buildings toggle. Its off-edge is a teardown of
    // exactly the same shape as a re-anchor — retire the far cells and the detail
    // patch through the shared despawn queue, drop their colliders, orphan both
    // in-flight sinks — so it joins the condition below rather than repeating it.
    // `anchor = None` there is what makes switching back on re-establish cleanly.
    let enabled = buildings_user_enabled();
    let turned_off = osm.enabled_was && !enabled;
    osm.enabled_was = enabled;

    let reanchored = osm.far.sync(stream.anchor_generation);
    if reanchored || turned_off || osm.terrain_revision != terrain.revision {
        osm.terrain_revision = terrain.revision;
        // Through the bounded queue, like everything else this lane retires. The
        // detail patch is one merged multi-megabyte mesh plus its windows, and
        // freeing it in the same command buffer as the far ring is precisely the
        // mass-despawn burst `platform::stream` owns a queue to prevent.
        let mut retired: Vec<Entity> = osm.detail_entities.drain(..).collect();
        retired.extend(osm.far_cells.drain().flat_map(|(_, ents)| ents));
        osm.far.retire_all(retired);
        osm.buildings.clear();
        colliders.clear_detail();
        osm.last_center = None;
        osm.last_fetch = None;
        osm.in_flight = false;
        // Orphans *all three* in-flight sinks, so nothing from the old origin leaks
        // through. The detail patch's own sink is the one that used to be missed,
        // and it was the one that mattered most: `far`/`far_tdbag` are reset by
        // `CellStream::reset`, but `fetch` is a plain `Arc` this module owns, so a
        // patch baked against the old `ax`/`ay`/`z`/`tile_m` sailed past the
        // teardown and was applied under the new anchor. The mesh then carried the
        // old projection while the colliders published beside it were re-projected
        // with the new one — so the buildings you could see and the buildings you
        // could walk into were in different places. Replacing the `Arc` is what
        // `reset` does for the other two, and it costs an allocation per re-anchor.
        osm.fetch = Arc::default();
        osm.far.reset();
        osm.far_tdbag.reset();
        osm.anchor = None;
    }

    if !enabled {
        // Everything past here establishes an anchor and streams cells. The
        // despawn drain above still runs each frame, so a ring retired by the
        // off-edge clears at its per-frame cap rather than in one command buffer.
        return;
    }

    // Establish the anchor (from the map origin). Far cells then stream one
    // CACHE_ZOOM cell at a time (disk-cached Overpass fetch + LOD mesh build),
    // entirely off the main thread.
    if osm.anchor.is_none() {
        let z = base_zoom;
        let (ax, ay) = lat_lng_to_tile(clat, clng, z);
        let lat = clat;
        osm.anchor = Some((ax, ay, z, lat));
    }
    let (ax, ay, z, lat) = osm.anchor.unwrap();
    let tile_m = tile_to_meters(z, lat) as f32;

    // Camera's current tile (world position → tile offset from the anchor).
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    // Freeze the building patch from orbit (ADR-025), same as map streaming:
    // otherwise flying to space drags the fetch centre far from the anchor,
    // despawning every local building and re-fetching an empty area — so
    // buildings would flicker out and back on each space trip. Geocentric
    // altitude, not local y (wrong on the far side of the globe).
    let cam_altitude = globe.altitude_m(cell, tf) as f32;
    if cam_altitude > crate::systems::globe::GLOBE_SHOW_ALT {
        return;
    }
    let world_x = cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x;
    let world_z = cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z;
    let center = (
        ax + (world_x / tile_m).round() as i64,
        ay + (world_z / tile_m).round() as i64,
    );

    // A 3DBAG toggle flip — or the coverage manifests resolving — invalidates
    // the standing patch: its building set was baked under the other setting
    // (covered cells filtered out, or present), so waiting for the drift
    // trigger would leave it stale indefinitely — stacked under the real
    // meshes after enabling, holed after disabling.
    let tdbag_now = (tdbag_user_enabled(), coverage_resolved());
    if osm.tdbag_was != Some(tdbag_now) {
        osm.tdbag_was = Some(tdbag_now);
        osm.last_center = None;
    }

    // Kick off a re-fetch when the camera has drifted far enough — but never
    // above `DETAIL_FREEZE_ALT` (a zoom-out drags `center` sideways fast; the
    // rebuild storm that triggered is what hung the GPU — see the const), and
    // never inside the cooldown window. The existing patch stays up while
    // frozen; descending below the threshold refreshes it at the new spot.
    let need = match osm.last_center {
        None => true,
        Some((lx, ly)) => (center.0 - lx).abs() + (center.1 - ly).abs() >= REFETCH_TILES,
    };
    let cooled = osm
        .last_fetch
        .map(|t| t.elapsed() >= DETAIL_REFETCH_COOLDOWN)
        .unwrap_or(true);
    // Over a LoD2.2-covered cell the real meshes (lod22.rs) carry the near
    // detail, so skip the extruded detail patch there entirely. `center` is in
    // base-zoom tile coords; coverage is keyed on CACHE_ZOOM cells, so convert.
    let cam_shift = z.saturating_sub(CACHE_ZOOM);
    let cam_cell14 = (
        center.0.div_euclid(1i64 << cam_shift),
        center.1.div_euclid(1i64 << cam_shift),
    );
    let over_lod22 = lod22_covers(cam_cell14.0, cam_cell14.1);
    // Entering LoD2.2 territory from outside: despawn any lingering extruded
    // detail patch (it only refreshes via a new fetch, which we now skip here) so
    // it doesn't shine through the real meshes.
    if over_lod22 && !osm.detail_entities.is_empty() {
        // Through the bounded queue, not inline: this is one merged multi-megabyte
        // mesh plus its windows, and freeing it in a single command buffer is the
        // mass-despawn burst `platform::stream` owns a queue to prevent.
        let retired: Vec<Entity> = osm.detail_entities.drain(..).collect();
        osm.far.retire_all(retired);
        osm.last_center = None;
    }
    // `coverage_resolved` holds the FIRST fetch those few startup frames: a
    // patch fetched before the manifests land bakes covered-cell buildings in
    // (the filter sees no coverage) and is refetched on resolution anyway —
    // one whole wasted Overpass bbox round-trip and mesh rebuild.
    if need
        && !osm.in_flight
        && cooled
        && cam_altitude < DETAIL_FREEZE_ALT
        && !over_lod22
        && coverage_resolved()
    {
        osm.in_flight = true;
        osm.last_center = Some(center);
        osm.last_fetch = Some(Instant::now());
        let slot = osm.fetch.clone();

        // Detail bounding box; the fetch assembles it from fixed CACHE_ZOOM
        // cells shared with the far-cell worker's disk cache.
        let rd = building_patch_radius();
        let (d_north, d_west) = tile_to_lat_lng(center.0 - rd, center.1 - rd, z);
        let (d_south, d_east) = tile_to_lat_lng(center.0 + rd + 1, center.1 + rd + 1, z);

        // The fetch is awaited on the task pool; the parse and the mesh build
        // (merging thousands of extrusions into shared vertex buffers) go to
        // `platform::workers`. This comment used to claim all of it already ran
        // "on the worker" — it did not, and that was the bug: `IoTaskPool` is the
        // main thread on wasm, so the hitch it describes was every frame, not an
        // occasional one. Vertices stay relative to the patch's BigSpace cell so
        // f32 remains precise at any range.
        let (patch_cell, _) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
            .translation_to_grid(Vec3::new(world_x, 0.0, world_z).as_dvec3());
        // The ground travels with the job (see `MeshData::set_base`).
        let ground = terrain.snapshot();
        IoTaskPool::get()
            .spawn(async move {
                let texts = fetch_patch_texts(d_south, d_west, d_north, d_east).await;
                crate::platform::workers::submit(Box::new(move || {
                    let cell_ox = patch_cell.x as f32 * WORLD_CELL_EDGE;
                    let cell_oz = patch_cell.z as f32 * WORLD_CELL_EDGE;
                    let result = Ok::<_, String>(parse_patch(
                        &texts, d_south, d_west, d_north, d_east,
                    ))
                    .map(|mut buildings| {
                        // 3DBAG territory: the real meshes own covered cells, and the
                        // patch (±BUILDING_PATCH_RADIUS base-zoom tiles) always sits
                        // inside the LoD2.2 keep ring, so covered cells here have real
                        // geometry — an extrusion would stack under or poke through
                        // it. This bites when the camera stands just OUTSIDE coverage
                        // and the patch reaches across the edge (the camera-cell skip
                        // below can't help there). Filtering the Vec, not the mesh
                        // loop, keeps hit-testing consistent with what is drawn.
                        buildings.retain(|b| {
                            b.ring.first().is_none_or(|&(la, lo)| {
                                let (bx, by) = lat_lng_to_tile(la, lo, CACHE_ZOOM);
                                !lod22_covers(bx, by)
                            })
                        });
                        // Deduped: with consistent windings the fan-triangulated flat caps
                        // and shared footprint corners collapse to one vertex each.
                        let mut bld = MeshData::with_dedup();
                        let mut wins: [MeshData; 4] = std::array::from_fn(|_| MeshData::default());
                        for b in &buildings {
                            let ring: Vec<Vec2> = b
                                .ring
                                .iter()
                                .map(|&(la, lo)| {
                                    let w = lat_lng_to_local(la, lo, ax, ay, z, tile_m);
                                    Vec2::new(w.x - cell_ox, w.y - cell_oz)
                                })
                                .collect();
                            // Stand it on the measured ground, one height for the whole
                            // footprint (see `MeshData::set_base`).
                            let base = ground
                                .level_over(ring.iter().map(|p| [p.x + cell_ox, p.y + cell_oz]));
                            bld.set_base(base);
                            wins.iter_mut().for_each(|w| w.set_base(base));
                            push_building(
                                &mut bld,
                                &mut wins,
                                b.id as u64,
                                &ring,
                                b.height_m,
                                b.height_is_eaves,
                                b.wall_color,
                                b.roof_color,
                                b.roof_shape,
                                b.roof_height,
                                b.roof_across,
                            );
                        }
                        BuiltPatch {
                            buildings,
                            patch_cell,
                            detail: bld.into_mesh(),
                            windows: wins.map(MeshData::into_mesh),
                        }
                    });
                    if let Ok(mut g) = slot.lock() {
                        *g = Some(result);
                    }
                }));
            })
            .detach();
    }

    let Ok(root) = bigspace.single() else {
        return;
    };

    // --- Far LOD cells: stream one CACHE_ZOOM cell at a time (like map tiles) ---
    {
        // Camera's CACHE_ZOOM cell from its base-zoom tile coords.
        let shift = z.saturating_sub(CACHE_ZOOM);
        let (ccx, ccy) = (
            center.0.div_euclid(1i64 << shift),
            center.1.div_euclid(1i64 << shift),
        );

        // Queue missing cells, nearest first so the surroundings fill outward.
        // Nothing queues until the coverage manifests resolve (a few startup
        // frames): queued earlier, covered cells route to the serial Overpass
        // lane — 800 ms courtesy sleep per cell — only to be evicted when the
        // real meshes arrive (see `coverage_resolved`).
        let mut to_request: Vec<(i64, i64)> = Vec::new();
        let queue_open = coverage_resolved();
        let far_r = far_cell_radius();
        for dy in -far_r..=far_r {
            if !queue_open {
                break;
            }
            for dx in -far_r..=far_r {
                let cell = (ccx + dx, ccy + dy);
                // Distance LOD: within the LoD2.2 KEEP ring (its request radius
                // plus its one cell of despawn hysteresis) the real meshes
                // (lod22.rs) own the cell, so skip the simplified far extrusion.
                // Matching the request radius instead left ring
                // `LOD22_RADIUS + 1` owned by both: lod22 still held its mesh
                // there while this tier fetched an extrusion under it — doubled
                // buildings all along the boundary as cells drifted through.
                let lod22_near = lod22_covers(cell.0, cell.1)
                    && (cell.0 - ccx).abs().max((cell.1 - ccy).abs())
                        <= crate::systems::lod22::lod22_radius() + 1;
                if !lod22_near {
                    to_request.push(cell);
                }
            }
        }
        to_request.sort_by_key(|c| (c.0 - ccx).abs().max((c.1 - ccy).abs()));
        // `CellStream::request` de-dups and is also the retry pump — it refuses a
        // cell already known or one that has spent its attempts.
        let now_secs = time.elapsed_secs_f64();
        for cell in to_request {
            // Two lanes: covered cells hit our own 3DBAG host and run
            // concurrently; everything else rides the Overpass lane. Routed at
            // request time, and the lane's own bookkeeping is what remembers it.
            let tdbag = tdbag_covers(cell.0, cell.1);
            osm.far_lane(tdbag).request(cell, now_secs);
        }

        // Dispatch queued far cells as detached tasks (ADR-033 seam) — no worker
        // thread. Both lanes drain here; each has its own bound and its own sink.
        let mut jobs: Vec<(bool, (i64, i64))> = Vec::new();
        while let Some(cell) = osm.far.next_job(now_secs) {
            jobs.push((false, cell));
        }
        while let Some(cell) = osm.far_tdbag.next_job(now_secs) {
            jobs.push((true, cell));
        }
        let sinks = (osm.far.sink(), osm.far_tdbag.sink());
        for (tdbag, (cx, cy)) in jobs {
            let results = if tdbag {
                sinks.1.clone()
            } else {
                sinks.0.clone()
            };
            // The ground travels with the job — footprints only exist in there.
            let ground = terrain.snapshot();
            // Fetch on the task pool, parse + mesh on the worker pool (ADR-033):
            // on wasm `IoTaskPool` is the main thread, so this whole thing used to
            // run on the frame loop.
            IoTaskPool::get()
                .spawn(async move {
                    match fetch_cell_text(cx, cy, CACHE_ZOOM).await {
                        Ok(text) => {
                            crate::platform::workers::submit(Box::new(move || {
                                let built =
                                    build_far_from_text(&text, cx, cy, ax, ay, z, lat, &ground);
                                if let Ok(mut g) = results.lock() {
                                    g.push(((cx, cy), built));
                                }
                            }));
                        }
                        Err(e) => {
                            if let Ok(mut g) = results.lock() {
                                g.push(((cx, cy), Err(e)));
                            }
                        }
                    }
                })
                .detach();
        }

        // Apply completed cells, at most `MAX_FAR_BUILDS_PER_FRAME` of them.
        //
        // This drain was uncapped, and it is the one hole left in the guard every
        // other streamer in this tree has. Both lanes run four in flight, so a
        // ring filling after a re-anchor could land **eight merged building meshes
        // plus their window meshes in a single frame** — which is precisely the
        // burst shape the module doc blames for `VK_ERROR_DEVICE_LOST` on a weak
        // iGPU, and the reason `lod22`, `map_stream`, `map_geometry` and `globe`
        // each have a cap already.
        let mut done: Vec<(bool, ((i64, i64), Result<Option<FarCellBuild>, String>))> = osm
            .far
            .take_results()
            .into_iter()
            .map(|r| (false, r))
            .chain(osm.far_tdbag.take_results().into_iter().map(|r| (true, r)))
            .collect();
        let cap = crate::quality::budget().per_frame(MAX_FAR_BUILDS_PER_FRAME);
        let deferred = done.split_off(done.len().min(cap));
        // Each deferred result goes back to the lane it came from — they are two
        // independent `CellStream`s, and a result returned to the wrong one would
        // release a concurrency slot that was never taken while holding one that
        // was, which drifts both lanes' in-flight counts apart for the run.
        let (tdbag_deferred, osm_deferred): (Vec<_>, Vec<_>) =
            deferred.into_iter().partition(|(tdbag, _)| *tdbag);
        osm.far
            .defer(osm_deferred.into_iter().map(|(_, r)| r).collect());
        osm.far_tdbag
            .defer(tdbag_deferred.into_iter().map(|(_, r)| r).collect());
        for (tdbag, (cell, built)) in done {
            match built {
                Ok(Some((grid_cell, mesh, windows))) => {
                    let material = osm
                        .far_material
                        .get_or_insert_with(|| {
                            materials.add(StandardMaterial {
                                base_color: Color::WHITE,
                                perceptual_roughness: 0.95,
                                double_sided: !BACKFACE_CULL,
                                cull_mode: building_cull_mode(),
                                ..default()
                            })
                        })
                        .clone();
                    let mut entities = vec![commands
                        .spawn((
                            Mesh3d(meshes.add(mesh)),
                            MeshMaterial3d(material),
                            Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)),
                            grid_cell,
                            ChildOf(root),
                            OsmBuildingMesh,
                            // Far cells are 1–5 km out: at the cascades'
                            // 2048²/3 km settings their shadows are sub-texel,
                            // but their double-sided (cull_mode None) geometry
                            // is most of what up to 8 night cascade passes
                            // would rasterize. Pure waste — skip it.
                            bevy::light::NotShadowCaster,
                            // From orbit the patch reads as a grey blob over
                            // the anchor; hidden with the tiles (ADR-025).
                            crate::components::HideInSpace,
                        ))
                        .id()];
                    // Window meshes: same recipe as the detail patch, but the four
                    // bucket materials are shared across every far cell.
                    let win_mats = osm
                        .far_window_materials
                        .get_or_insert_with(|| {
                            std::array::from_fn(|_| {
                                materials.add(StandardMaterial {
                                    base_color: Color::srgb(0.07, 0.09, 0.13),
                                    emissive: LinearRgba::BLACK,
                                    perceptual_roughness: 0.25,
                                    double_sided: true,
                                    cull_mode: None,
                                    ..default()
                                })
                            })
                        })
                        .clone();
                    for (i, w) in windows.into_iter().enumerate() {
                        let Some(mesh) = w else {
                            continue;
                        };
                        entities.push(
                            commands
                                .spawn((
                                    Mesh3d(meshes.add(mesh)),
                                    MeshMaterial3d(win_mats[i].clone()),
                                    Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)),
                                    grid_cell,
                                    ChildOf(root),
                                    WindowGlow(WINDOW_GLOW_MULT[i]),
                                    // Proud-of-facade quads; see the detail-patch
                                    // window spawn — never worth shadow-casting.
                                    bevy::light::NotShadowCaster,
                                    crate::components::HideInSpace,
                                ))
                                .id(),
                        );
                    }
                    osm.far_cells.insert(cell, entities);
                    osm.far_lane(tdbag).succeeded(&cell);
                }
                // Empty cell; stays requested so it isn't re-queried.
                Ok(None) => {
                    osm.far_lane(tdbag).succeeded(&cell);
                }
                Err(e) => {
                    tracing::warn!(cell = ?cell, error = %e, "far building cell failed");
                    // Allows a bounded re-queue on a later frame.
                    osm.far_lane(tdbag).failed(&cell, now_secs);
                }
            }
        }

        // Despawn cells out of range (with one cell of hysteresis); re-entry
        // re-requests them, served from the disk cache. Covered cells inside
        // the LoD2.2 keep ring go too: the real meshes own those, and an
        // extrusion there is a leftover from the coverage race or a 3DBAG
        // toggle flip, standing stacked under the real geometry.
        let stale: Vec<(i64, i64)> = osm
            .far_cells
            .keys()
            .filter(|(cx2, cy2)| {
                let d = (cx2 - ccx).abs().max((cy2 - ccy).abs());
                d > far_cell_radius() + 1
                    || (d <= crate::systems::lod22::lod22_radius() + 1 && lod22_covers(*cx2, *cy2))
            })
            .copied()
            .collect();
        for cell in stale {
            let entities = osm.far_cells.remove(&cell).unwrap_or_default();
            // Retiring is lane-agnostic (the `far` lane owns the queue); forgetting
            // is not, so both are asked — only the owning lane has anything to drop.
            osm.far.retire_all(entities);
            osm.far.forget(&cell);
            osm.far_tdbag.forget(&cell);
        }
    }

    // --- Detail patch: apply a completed fetch+build ---
    let ready = osm.fetch.lock().ok().and_then(|mut g| g.take());
    let Some(result) = ready else {
        return;
    };
    osm.in_flight = false;
    let patch = match result {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "OSM buildings fetch failed");
            return;
        }
    };
    // The patch swap, and the last inline mass-despawn in this module. The
    // outgoing patch is a merged mesh over up to several thousand buildings; the
    // incoming one is uploaded in this same frame a few lines below, so freeing
    // the old one inline put an unbounded free and a multi-megabyte upload in one
    // command buffer — the shape CLAUDE.md names as the original
    // `VK_ERROR_DEVICE_LOST` trigger.
    let retired: Vec<Entity> = osm.detail_entities.drain(..).collect();
    osm.far.retire_all(retired);
    let patch_cell = patch.patch_cell;
    let count = patch.buildings.len();
    osm.buildings = patch.buildings; // retain footprints for future per-building conversion
                                     // Publish footprints (exact OSM rings → world-XZ polygons) for avatar
                                     // collision. `ax/ay/z/tile_m` are the anchor established above, so these land
                                     // in the same frame as the avatar's absolute position.
    let detail_fps = osm
        .buildings
        .iter()
        .filter_map(|b| {
            let ring: Vec<Vec2> = b
                .ring
                .iter()
                .map(|&(la, lo)| lat_lng_to_local(la, lo, ax, ay, z, tile_m))
                .collect();
            crate::systems::collision::Footprint::new(ring)
        })
        .collect();
    colliders.set_detail(detail_fps);
    spawn_detail_patch(
        &mut commands,
        &mut meshes,
        &mut materials,
        &mut osm,
        root,
        patch_cell,
        patch.detail,
        patch.windows,
    );
    tracing::info!(count, "buildings updated");
}

/// A far cell's built output: its BigSpace grid cell, the merged building mesh,
/// and the sparse lit-window mesh per glow bucket.
type FarCellBuild = (GridCell, Mesh, [Option<Mesh>; 4]);

/// Fetch one CACHE_ZOOM cell (disk-cached Overpass) and build its far-LOD meshes
/// (buildings + windows), anchored to the cell's own BigSpace grid cell for f32
/// precision. Runs on the far-cell worker thread. Returns `None` for cells with
/// no buildings.
/// The CPU half of a far cell: response text in, meshes out. Pure and owned, so
/// it is `Send` and runs on `platform::workers` rather than on the thread that
/// renders — see `build_lod22_from_text` for the measurement that motivated it.
fn build_far_from_text(
    text: &str,
    cx: i64,
    cy: i64,
    ax: i64,
    ay: i64,
    z: u32,
    lat: f64,
    ground: &crate::systems::terrain::TerrainSnapshot,
) -> Result<Option<FarCellBuild>, String> {
    let buildings = parse_buildings(text)?;
    let (north, west) = tile_to_lat_lng(cx, cy, CACHE_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, CACHE_ZOOM);
    let tile_m = tile_to_meters(z, lat) as f32;

    // Anchor vertices to the BigSpace cell at the cell's centre.
    let c_world = lat_lng_to_local(
        (north + south) / 2.0,
        (west + east) / 2.0,
        ax,
        ay,
        z,
        tile_m,
    );
    let (grid_cell, _) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
        .translation_to_grid(Vec3::new(c_world.x, 0.0, c_world.y).as_dvec3());
    let ox = grid_cell.x as f32 * WORLD_CELL_EDGE;
    let oz = grid_cell.z as f32 * WORLD_CELL_EDGE;

    // Deduped — see the far-cell merge.
    let mut buf = MeshData::with_dedup();
    let mut wins: [MeshData; 4] = std::array::from_fn(|_| MeshData::default());
    for b in &buildings {
        // A building straddling a cell edge is returned by both cells; only the
        // cell owning its first vertex renders it. Half-open bounds ([south,north),
        // [west,east)) so a vertex exactly on a shared edge lands in one cell only.
        let (blat, blng) = b.ring[0];
        if blat < south || blat >= north || blng < west || blng >= east {
            continue;
        }
        let ring: Vec<Vec2> = b
            .ring
            .iter()
            .map(|&(la, lo)| {
                let w = lat_lng_to_local(la, lo, ax, ay, z, tile_m);
                Vec2::new(w.x - ox, w.y - oz)
            })
            .collect();
        // Sheds and garages read as noise from afar; the detail ring shows them
        // up close, and skipping them keeps the vertex budget in check.
        if polygon_area(&ring) < 30.0 {
            continue;
        }
        // Cap the LOD box at the detail mesh's eaves (its roof's lowest point) so
        // the far volume sits *under* its detailed twin in the overlap region,
        // instead of a flat slab poking up through the pitched roof (Stage 1).
        let eaves = detail_eaves(b, &ring);
        let base = ground.level_over(ring.iter().map(|p| [p.x + ox, p.y + oz]));
        buf.set_base(base);
        wins.iter_mut().for_each(|w| w.set_base(base));
        push_building_lod(
            &mut buf,
            &mut wins,
            b.id as u64,
            &ring,
            eaves,
            b.wall_color,
            b.roof_color,
        );
    }
    Ok(buf
        .into_mesh()
        .map(|m| (grid_cell, m, wins.map(MeshData::into_mesh))))
}

/// Spawn the detail-ring meshes (buildings + per-bucket windows) for a completed
/// detail patch, tracking the entities for despawn on the next refresh.
#[allow(clippy::too_many_arguments)]
fn spawn_detail_patch(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    osm: &mut OsmBuildings,
    root: Entity,
    patch_cell: GridCell,
    detail: Option<Mesh>,
    windows: [Option<Mesh>; 4],
) {
    // Building mesh: one white material, per-vertex wall/roof colours.
    if let Some(mesh) = detail {
        let material = materials.add(StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 0.95,
            double_sided: !BACKFACE_CULL,
            cull_mode: building_cull_mode(),
            ..default()
        });
        let entity = commands
            .spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material),
                Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)),
                patch_cell,
                ChildOf(root),
                OsmBuildingMesh,
                crate::components::HideInSpace,
            ))
            .id();
        osm.detail_entities.push(entity);
    }

    // Window meshes: dark glass by day; per-bucket emissive faded in at night by
    // `update_window_glow`, so windows vary (off / dim / bright) across buildings.
    for (i, w) in windows.into_iter().enumerate() {
        let Some(mesh) = w else {
            continue;
        };
        let material = materials.add(StandardMaterial {
            base_color: Color::srgb(0.07, 0.09, 0.13),
            emissive: LinearRgba::BLACK,
            perceptual_roughness: 0.25,
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        let entity = commands
            .spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material),
                Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)),
                patch_cell,
                ChildOf(root),
                WindowGlow(WINDOW_GLOW_MULT[i]),
                // Windows sit 6 cm proud of the facade that already casts the
                // shadow — rendering their (huge) quad soup into up to 8 cascade
                // maps buys nothing.
                bevy::light::NotShadowCaster,
                crate::components::HideInSpace,
            ))
            .id();
        osm.detail_entities.push(entity);
    }
}

/// Web Mercator (lat,lng) → world metres relative to the anchor tile centre.
/// `tile_m` is the real ground edge of a tile; East is `+X`, South is `+Z`.
pub(crate) fn lat_lng_to_local(
    lat: f64,
    lng: f64,
    tile_x: i64,
    tile_y: i64,
    z: u32,
    tile_m: f32,
) -> Vec2 {
    let n = 2.0_f64.powi(z as i32);
    let x = (lng + 180.0) / 360.0 * n;
    let lat_rad = lat.to_radians();
    let y = (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n;

    let fx = (x - tile_x as f64) as f32;
    let fy = (y - tile_y as f64) as f32;
    Vec2::new((fx - 0.5) * tile_m, (fy - 0.5) * tile_m)
}

/// An accumulating triangle-mesh buffer.
#[derive(Default)]
pub(crate) struct MeshData {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
    /// When set, [`MeshData::vertex`] reuses an existing vertex whose position,
    /// normal *and* colour all match instead of appending a fresh one. See
    /// [`MeshData::with_dedup`].
    dedup: Option<HashMap<VertexKey, u32, BuildHasherDefault<VertexHasher>>>,
    /// Ground offset added to every vertex pushed — see [`MeshData::set_base`].
    base_y: f32,
}

/// A vertex's position, flat normal and colour, as raw bit patterns.
type VertexKey = [u32; 10];

/// Hasher for [`VertexKey`], replacing the default `HashMap`'s SipHash-1-3.
///
/// The map itself is not the problem — hashbrown probes its control bytes
/// sixteen at a time with SSE2, which is as good as this gets. The problem is
/// that it never gets to: SipHash is a serial ARX construction over all 40
/// bytes, and at three lookups per triangle it costs more than the probe it
/// feeds. A dense LoD2.2 cell spends ~34 ms in here; this brings it to ~18 ms
/// for the same 720k vertices.
///
/// Standard `FxHash` (rustc's own), which is built for exactly this shape of
/// key: short, fixed-size, already well-distributed integers. One multiply and
/// one rotate per 8 bytes, five rounds for the whole key.
///
/// The tradeoff is HashDoS resistance, and it is the right one to make here.
/// The keys are float bit patterns from geometry, the map is local to one cell's
/// mesh build and dropped with it, and a worst-case adversarial tile buys an
/// attacker a slow mesh — never a wrong one, because `HashMap` still compares
/// keys on every probe hit. Correctness does not depend on the hash at all.
#[derive(Default)]
pub(crate) struct VertexHasher(u64);

impl std::hash::Hasher for VertexHasher {
    fn write(&mut self, bytes: &[u8]) {
        // `[u32; N]` hashes through `Hash::hash_slice`, which hands the whole
        // array over as one byte slice — so this is the only method that runs.
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.mix(u64::from_le_bytes(c.try_into().expect("chunks_exact(8)")));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            self.mix(u64::from_le_bytes(buf));
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl VertexHasher {
    /// FxHash's round constant: the odd 64-bit fractional part of the golden
    /// ratio, so the multiply spreads low bits into high ones.
    const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

    #[inline]
    fn mix(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(Self::SEED);
    }
}

impl MeshData {
    /// A buffer that shares identical vertices between triangles.
    ///
    /// For geometry assembled triangle-by-triangle from an already-indexed source
    /// — the LoD2.2 cell merge, which receives deduplicated vertices from the host
    /// and would otherwise re-push three fresh ones per triangle — this is a large
    /// win: a dense cell went from 3 vertices per triangle plus an identity index
    /// buffer (which offers the post-transform cache nothing to hit) to genuinely
    /// shared corners.
    ///
    /// Lossless by construction: vertices merge only when position, flat normal and
    /// colour are bit-identical, which is exactly the coplanar-same-colour case
    /// (fan-triangulated flat roofs, earcut output) where the duplicates were
    /// redundant anyway. Anything with a normal or colour discontinuity — every
    /// wall/roof crease — keeps its separate vertices, so shading is unchanged.
    pub(crate) fn with_dedup() -> Self {
        Self {
            dedup: Some(HashMap::default()),
            ..Default::default()
        }
    }

    /// Lift every vertex pushed from now on by `base` metres — the measured ground
    /// under the building about to be written (`systems::terrain`).
    ///
    /// A whole-building offset applied at the buffer, rather than threaded through
    /// the dozen y-coordinates a wall, roof, ridge and window row each compute, and
    /// applied *before* the dedup key so a lifted corner can never be merged with an
    /// identically-shaped one on different ground.
    pub(crate) fn set_base(&mut self, base: f32) {
        self.base_y = base;
    }

    /// Append (or, in dedup mode, look up) one vertex and return its index.
    fn vertex(&mut self, v: Vec3, n: Vec3, color: [f32; 4]) -> u32 {
        let v = Vec3::new(v.x, v.y + self.base_y, v.z);
        // Hash the bit patterns, not the floats: these are copied values, so equal
        // corners are bit-equal. (NaN can't arise here — `normalize_or_zero`.)
        let key: Option<VertexKey> = self.dedup.is_some().then(|| {
            [
                v.x.to_bits(),
                v.y.to_bits(),
                v.z.to_bits(),
                n.x.to_bits(),
                n.y.to_bits(),
                n.z.to_bits(),
                color[0].to_bits(),
                color[1].to_bits(),
                color[2].to_bits(),
                color[3].to_bits(),
            ]
        });
        if let (Some(k), Some(map)) = (&key, &self.dedup) {
            if let Some(&i) = map.get(k) {
                return i;
            }
        }
        let idx = self.positions.len() as u32;
        self.positions.push([v.x, v.y, v.z]);
        self.normals.push([n.x, n.y, n.z]);
        self.colors.push(color);
        if let (Some(k), Some(map)) = (key, &mut self.dedup) {
            map.insert(k, idx);
        }
        idx
    }

    /// Push a triangle with a flat computed normal.
    pub(crate) fn tri(&mut self, a: Vec3, b: Vec3, c: Vec3, color: [f32; 4]) {
        let n = (b - a).cross(c - a).normalize_or_zero();
        let ia = self.vertex(a, n, color);
        let ib = self.vertex(b, n, color);
        let ic = self.vertex(c, n, color);
        self.indices.extend_from_slice(&[ia, ib, ic]);
    }

    /// Push a planar quad (a→b→c→d) as two triangles over **four** shared vertices.
    ///
    /// Not `tri(a,b,c) + tri(a,c,d)`: that emits six vertices and recomputes the
    /// same flat normal twice. Every window pane goes through here and windows are
    /// the highest-count geometry in the scene, so the 33% saving on
    /// position/normal/colour bytes — and on vertex-shader invocations, in the main
    /// view *and* in each of the up-to-8 shadow cascades — is worth the duplication
    /// of `tri`'s body. Output is pixel-identical: all callers pass planar quads, so
    /// the single normal is the one both triangles would have computed anyway.
    pub(crate) fn quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3, color: [f32; 4]) {
        let n = (b - a).cross(c - a).normalize_or_zero();
        let ia = self.vertex(a, n, color);
        let ib = self.vertex(b, n, color);
        let ic = self.vertex(c, n, color);
        let id = self.vertex(d, n, color);
        self.indices.extend_from_slice(&[ia, ib, ic, ia, ic, id]);
    }

    /// Push a triangle wound so its face normal points along `outward`.
    ///
    /// The generators below can't guarantee a consistent winding on their own:
    /// earcut preserves whatever orientation the OSM ring happened to have, and the
    /// oriented-bounding-box basis can come out either handedness. That's why the
    /// building materials were `double_sided` with `cull_mode: None` — which costs
    /// rasterizing every back face, in the main view *and* in each shadow cascade.
    ///
    /// Rather than hand-orienting each generator, every call states the direction
    /// the surface *faces* — `Vec3::Y` for any roof, the outward wall normal for a
    /// wall — and the winding follows. Note this fixes shading too: `tri` derives
    /// its flat normal from the winding, so inward-wound roofs were previously
    /// lit via `double_sided`'s normal flip rather than being correct.
    pub(crate) fn tri_facing(&mut self, a: Vec3, b: Vec3, c: Vec3, color: [f32; 4], outward: Vec3) {
        if (b - a).cross(c - a).dot(outward) < 0.0 {
            self.tri(a, c, b, color);
        } else {
            self.tri(a, b, c, color);
        }
    }

    /// [`MeshData::tri_facing`] for a planar quad (a→b→c→d).
    pub(crate) fn quad_facing(
        &mut self,
        a: Vec3,
        b: Vec3,
        c: Vec3,
        d: Vec3,
        color: [f32; 4],
        outward: Vec3,
    ) {
        if (b - a).cross(c - a).dot(outward) < 0.0 {
            self.quad(d, c, b, a, color);
        } else {
            self.quad(a, b, c, d, color);
        }
    }

    /// Finalise into a `Mesh` (or `None` if empty).
    ///
    /// `RENDER_WORLD` only, for the same reason `tile_loader::TILE_TEXTURE_USAGE`
    /// is: keeping `MAIN_WORLD` as well (which is what `default()` means) holds the
    /// vertex buffers alive in `Assets<Mesh>` for the lifetime of the handle *on
    /// top of* the copy on the GPU, and nothing here ever reads them back — a
    /// building mesh is built on a worker, uploaded, and only ever drawn. These are
    /// the largest meshes in the app: a merged 3DBAG cell (`lod22`) is ~10^5
    /// vertices at ~40 B each, with up to 7×7 cells live, so the duplicate is tens
    /// of megabytes of resident main-world memory that no code path can observe.
    ///
    /// The one visible consequence is diagnostic: `log_render_cost` counts an
    /// extracted mesh as zero triangles (it already does this for the tiles and the
    /// globe), so `ktris_buildings` under-reports. That is a known, deliberate
    /// trade — see the non-panicking accessors there.
    pub(crate) fn into_mesh(self) -> Option<Mesh> {
        if self.indices.is_empty() {
            return None;
        }
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        mesh.insert_indices(Indices::U32(self.indices));
        Some(mesh)
    }
}

/// Oriented bounding box of a footprint in a (u, v) basis (min-area over edges).
struct Obb {
    u: Vec2,
    v: Vec2,
    umin: f32,
    umax: f32,
    vmin: f32,
    vmax: f32,
    area: f32,
}

fn oriented_bbox(ring: &[Vec2]) -> Obb {
    let n = ring.len();
    let mut best: Option<Obb> = None;
    for i in 0..n {
        let e = (ring[(i + 1) % n] - ring[i]).normalize_or_zero();
        if e == Vec2::ZERO {
            continue;
        }
        let (u, v) = (e, Vec2::new(-e.y, e.x));
        let (mut umin, mut umax, mut vmin, mut vmax) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in ring {
            let (du, dv) = (p.dot(u), p.dot(v));
            umin = umin.min(du);
            umax = umax.max(du);
            vmin = vmin.min(dv);
            vmax = vmax.max(dv);
        }
        let area = (umax - umin) * (vmax - vmin);
        if best.as_ref().is_none_or(|b| area < b.area) {
            best = Some(Obb {
                u,
                v,
                umin,
                umax,
                vmin,
                vmax,
                area,
            });
        }
    }
    best.unwrap_or(Obb {
        u: Vec2::X,
        v: Vec2::Y,
        umin: 0.0,
        umax: 0.0,
        vmin: 0.0,
        vmax: 0.0,
        area: 0.0,
    })
}

fn polygon_area(ring: &[Vec2]) -> f32 {
    let n = ring.len();
    let mut a = 0.0;
    for i in 0..n {
        let p = ring[i];
        let q = ring[(i + 1) % n];
        a += p.x * q.y - q.x * p.y;
    }
    a.abs() * 0.5
}

/// Greedy outline simplification for far LOD: drop vertices that deviate less
/// than `tol` metres from the line between their kept neighbours. Far
/// silhouettes don't need sub-metre detail, and this roughly halves the ring's
/// vertex count on typical OSM footprints.
fn simplify_ring(ring: &[Vec2], tol: f32) -> Vec<Vec2> {
    let n = ring.len();
    if n <= 4 {
        return ring.to_vec();
    }
    let mut out: Vec<Vec2> = Vec::with_capacity(n);
    for i in 0..n {
        let prev = *out.last().unwrap_or(&ring[n - 1]);
        let cur = ring[i];
        let next = ring[(i + 1) % n];
        let ab = next - prev;
        let len = ab.length();
        let dist = if len < 1e-3 {
            tol + 1.0
        } else {
            (cur - prev).perp_dot(ab / len).abs()
        };
        if dist > tol {
            out.push(cur);
        }
    }
    if out.len() >= 3 {
        out
    } else {
        ring.to_vec()
    }
}

/// The eaves (wall-top) height the *detail* mesh uses for this building — the
/// lowest point of its roof. The far-LOD box is capped here so it always sits
/// under its detailed twin instead of poking a flat cap through the roof. Mirrors
/// the eaves computation in `push_building`.
fn detail_eaves(b: &Building, ring: &[Vec2]) -> f32 {
    if b.height_is_eaves {
        // Detail walls rise to the full tagged/estimated height; the roof adds on top.
        return b.height_m;
    }
    // Total-height building: the detail mesh carves the roof out of the top, so the
    // wall top is `height − roof_height`.
    let pitch = if b.roof_shape == RoofShape::Flat {
        0.0
    } else {
        let obb = oriented_bbox(ring);
        let short = (obb.umax - obb.umin).min(obb.vmax - obb.vmin);
        b.roof_height.unwrap_or((short * 0.35).clamp(1.5, 6.0))
    };
    let roof_h = pitch.min(b.height_m - 2.0).max(0.0);
    b.height_m - roof_h
}

/// Far-LOD building: a simplified plain extrusion with a flat cap and a sparse
/// grid of lit-window quads (`FAR_WINDOWS`) — no roof shapes. Merged into per-cell
/// meshes for the whole far ring. `height` is the detailed twin's *eaves* height
/// (its roof's lowest point); with the footprint inset toward the centroid and the
/// cap dropped just under that, the LOD volume nests strictly *inside* the detailed
/// version: the far mesh covers the detail area too (as a fallback while a patch
/// refresh is in flight) without poking through it, and its windows sit behind the
/// detail walls, so lights never double up.
fn push_building_lod(
    bld: &mut MeshData,
    wins: &mut [MeshData; 4],
    seed: u64,
    ring_in: &[Vec2],
    height: f32,
    wall_color: [f32; 3],
    roof_color: [f32; 3],
) {
    let mut ring = ring_in.to_vec();
    if ring.len() >= 2 && ring.first() == ring.last() {
        ring.pop();
    }
    let ring = simplify_ring(&ring, 0.5);
    let n = ring.len();
    if n < 3 || height <= 0.0 {
        return;
    }
    let wall = [wall_color[0], wall_color[1], wall_color[2], 1.0];
    let roof = [roof_color[0], roof_color[1], roof_color[2], 1.0];

    let centroid = ring.iter().copied().sum::<Vec2>() / n as f32;
    let ring: Vec<Vec2> = ring
        .iter()
        .map(|&p| {
            let to_c = centroid - p;
            let d = to_c.length();
            if d > 1.5 {
                p + to_c / d * 0.35
            } else {
                p
            }
        })
        .collect();
    let height = (height - 0.25).max(1.5);
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let mut perp = Vec2::new((b - a).y, -(b - a).x).normalize_or_zero();
        if perp.dot((a + b) * 0.5 - centroid) < 0.0 {
            perp = -perp;
        }
        // Wound to face `perp` so far walls back-face cull too (see `quad_facing`).
        bld.quad_facing(
            Vec3::new(a.x, 0.0, a.y),
            Vec3::new(b.x, 0.0, b.y),
            Vec3::new(b.x, height, b.y),
            Vec3::new(a.x, height, a.y),
            wall,
            Vec3::new(perp.x, 0.0, perp.y),
        );

        // Sparse lit windows so the night city keeps glowing past the detail ring;
        // kept a couple of metres below the wall top so they never reach the roof.
        let win_top = height - 2.5;
        if win_top >= WINDOW_MIN_HEIGHT {
            push_wall_windows(wins, seed, a, b, perp, win_top, &FAR_WINDOWS);
        }
    }
    flat_cap(bld, &ring, height, roof);
}

/// Append one building (walls up to the eaves + a shaped roof) into the buffers,
/// with distinct wall/roof vertex colours. Degenerate footprints are skipped.
#[allow(clippy::too_many_arguments)]
fn push_building(
    bld: &mut MeshData,
    wins: &mut [MeshData; 4],
    seed: u64,
    ring_in: &[Vec2],
    height: f32,
    height_is_eaves: bool,
    wall_color: [f32; 3],
    roof_color: [f32; 3],
    roof_shape: RoofShape,
    roof_height: Option<f32>,
    roof_across: bool,
) {
    let mut ring = ring_in.to_vec();
    if ring.len() >= 2 && ring.first() == ring.last() {
        ring.pop();
    }
    let n = ring.len();
    if n < 3 || height <= 0.0 {
        return;
    }
    let wall = [wall_color[0], wall_color[1], wall_color[2], 1.0];
    let roof = [roof_color[0], roof_color[1], roof_color[2], 1.0];

    let obb = oriented_bbox(&ring);
    let short = (obb.umax - obb.umin).min(obb.vmax - obb.vmin);

    // Roof pitch height: explicit tag, else ~35° over the short span.
    let pitch = if roof_shape == RoofShape::Flat {
        0.0
    } else {
        roof_height.unwrap_or((short * 0.35).clamp(1.5, 6.0))
    };
    // Eaves (wall top) and ridge (roof apex). For an eaves height the roof adds on
    // top; for a total height it's carved out of the top.
    let (wall_h, ridge_y) = if height_is_eaves {
        (height, height + pitch)
    } else {
        let rh = pitch.min(height - 2.0).max(0.0);
        (height - rh, height)
    };
    let roof_h = ridge_y - wall_h;
    let shape = if roof_h <= 0.1 {
        RoofShape::Flat
    } else {
        roof_shape
    };

    // --- Walls: footprint 0 → eaves, oriented outward via the centroid ---
    let centroid = ring.iter().copied().sum::<Vec2>() / n as f32;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let mut perp = Vec2::new((b - a).y, -(b - a).x).normalize_or_zero();
        if perp.dot((a + b) * 0.5 - centroid) < 0.0 {
            perp = -perp;
        }
        // Wound to face `perp`, so the wall can be back-face culled. (A vertical
        // quad's derived flat normal is exactly ±perp, so this is identical to the
        // explicit normal this used to push — just with the winding fixed too.)
        bld.quad_facing(
            Vec3::new(a.x, 0.0, a.y),
            Vec3::new(b.x, 0.0, b.y),
            Vec3::new(b.x, wall_h, b.y),
            Vec3::new(a.x, wall_h, a.y),
            wall,
            Vec3::new(perp.x, 0.0, perp.y),
        );

        // Windows only on the walls of tall enough buildings (skips houses/sheds
        // and all roof/gable surfaces).
        if wall_h >= WINDOW_MIN_HEIGHT {
            push_wall_windows(wins, seed, a, b, perp, wall_h, &DETAIL_WINDOWS);
        }
    }

    // --- Roof ---
    match shape {
        RoofShape::Flat => flat_cap(bld, &ring, wall_h, roof),
        RoofShape::Pyramidal => {
            let apex = Vec3::new(centroid.x, ridge_y, centroid.y);
            for i in 0..n {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                bld.tri_facing(
                    Vec3::new(a.x, wall_h, a.y),
                    Vec3::new(b.x, wall_h, b.y),
                    apex,
                    roof,
                    Vec3::Y,
                );
            }
        }
        RoofShape::Skillion => skillion_roof(bld, &ring, &obb, wall_h, roof_h, roof_across, roof),
        RoofShape::Gabled | RoofShape::Hipped => {
            // Ridged roofs assume a ~rectangular footprint; fall back to flat otherwise.
            if obb.area <= 0.0 || polygon_area(&ring) / obb.area < 0.82 {
                flat_cap(bld, &ring, wall_h, roof);
            } else {
                ridged_roof(
                    bld,
                    &obb,
                    wall_h,
                    ridge_y,
                    shape == RoofShape::Hipped,
                    roof_across,
                    roof,
                    wall,
                );
            }
        }
    }
}

/// Emit a grid of window quads on one wall face (proud of the wall), routing each
/// window into a brightness bucket so the lit pattern varies per building. The
/// grid config picks the LOD tier: dense real-size windows for the detail patch,
/// sparse oversized ones for far cells.
fn push_wall_windows(
    wins: &mut [MeshData; 4],
    seed: u64,
    a: Vec2,
    b: Vec2,
    out: Vec2,
    wall_h: f32,
    g: &WindowGrid,
) {
    let along = b - a;
    let len = along.length();
    if len < g.pitch {
        return;
    }
    let dir = along / len;
    let proud = out * g.proud;
    let ncols = (len / g.pitch).floor() as i32;
    if ncols < 1 {
        return;
    }
    let start = (len - (ncols as f32 - 1.0) * g.pitch) * 0.5;
    let color = [1.0, 1.0, 1.0, 1.0];
    let mut row = 0i32;
    let mut wy = g.sill;
    while wy + g.h <= wall_h - 0.4 {
        for c in 0..ncols {
            let bucket = window_bucket(seed, row, c);
            if bucket == 0 && g.skip_dark {
                continue;
            }
            let cx = start + c as f32 * g.pitch;
            let center = a + dir * cx;
            let half = dir * (g.w * 0.5);
            let (bl, br) = (center - half, center + half);
            wins[bucket].quad(
                Vec3::new(bl.x + proud.x, wy, bl.y + proud.y),
                Vec3::new(br.x + proud.x, wy, br.y + proud.y),
                Vec3::new(br.x + proud.x, wy + g.h, br.y + proud.y),
                Vec3::new(bl.x + proud.x, wy + g.h, bl.y + proud.y),
                color,
            );
        }
        wy += g.floor;
        row += 1;
    }
}

/// Deterministically bucket a window (per building + floor + column) into a
/// brightness level, weighted toward dark/dim so not every window blazes.
fn window_bucket(seed: u64, row: i32, col: i32) -> usize {
    let h =
        unit_rand(seed ^ ((row as u64) << 20) ^ (col as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    if h < 0.35 {
        0
    } else if h < 0.60 {
        1
    } else if h < 0.85 {
        2
    } else {
        3
    }
}

/// Triangulated flat cap at `y`.
fn flat_cap(buf: &mut MeshData, ring: &[Vec2], y: f32, color: [f32; 4]) {
    let flat: Vec<f64> = ring.iter().flat_map(|p| [p.x as f64, p.y as f64]).collect();
    let Ok(cap) = earcutr::earcut(&flat, &[], 2) else {
        return;
    };
    for t in cap.chunks_exact(3) {
        let p = |i: usize| Vec3::new(ring[i].x, y, ring[i].y);
        // Earcut keeps the input ring's orientation, which OSM doesn't guarantee —
        // orient by "a roof faces up" instead (see `tri_facing`).
        buf.tri_facing(p(t[0]), p(t[1]), p(t[2]), color, Vec3::Y);
    }
}

/// Single-plane mono-pitch roof: cap sloped linearly along the short axis.
fn skillion_roof(
    buf: &mut MeshData,
    ring: &[Vec2],
    obb: &Obb,
    wall_h: f32,
    roof_h: f32,
    across: bool,
    color: [f32; 4],
) {
    // Slope along the short axis by default (or long if `across`).
    let (axis, amin, amax) = if across {
        (obb.u, obb.umin, obb.umax)
    } else {
        (obb.v, obb.vmin, obb.vmax)
    };
    let span = (amax - amin).max(0.001);
    let lift = |p: Vec2| -> Vec3 {
        let t = (p.dot(axis) - amin) / span;
        Vec3::new(p.x, wall_h + roof_h * t, p.y)
    };
    let flat: Vec<f64> = ring.iter().flat_map(|p| [p.x as f64, p.y as f64]).collect();
    let Ok(cap) = earcutr::earcut(&flat, &[], 2) else {
        return;
    };
    for t in cap.chunks_exact(3) {
        // A mono-pitch plane still faces up, so `Vec3::Y` orients it (see `flat_cap`).
        buf.tri_facing(
            lift(ring[t[0]]),
            lift(ring[t[1]]),
            lift(ring[t[2]]),
            color,
            Vec3::Y,
        );
    }
}

/// Gabled (or hipped) roof over the oriented bounding box.
#[allow(clippy::too_many_arguments)]
fn ridged_roof(
    buf: &mut MeshData,
    obb: &Obb,
    wall_h: f32,
    ridge_y: f32,
    hipped: bool,
    across: bool,
    roof: [f32; 4],
    wall: [f32; 4],
) {
    let u_len = obb.umax - obb.umin;
    let v_len = obb.vmax - obb.vmin;
    // Ridge runs along the long axis by default; `across` flips it.
    let ridge_along_u = (u_len >= v_len) ^ across;
    // a = ridge axis, b = slope axis.
    let (a_ax, a0, a1, b_ax, b0, b1) = if ridge_along_u {
        (obb.u, obb.umin, obb.umax, obb.v, obb.vmin, obb.vmax)
    } else {
        (obb.v, obb.vmin, obb.vmax, obb.u, obb.umin, obb.umax)
    };
    let bc = (b0 + b1) * 0.5;
    let pt = |av: f32, bv: f32, y: f32| -> Vec3 {
        let xz = a_ax * av + b_ax * bv;
        Vec3::new(xz.x, y, xz.y)
    };

    // Eave corners at the walls.
    let e00 = pt(a0, b0, wall_h);
    let e10 = pt(a1, b0, wall_h);
    let e11 = pt(a1, b1, wall_h);
    let e01 = pt(a0, b1, wall_h);

    // Ridge: full length for gabled; inset by the half-span for hipped.
    let inset = if hipped {
        ((b1 - b0) * 0.5).min((a1 - a0) * 0.5)
    } else {
        0.0
    };
    let r0 = pt(a0 + inset, bc, ridge_y);
    let r1 = pt(a1 - inset, bc, ridge_y);

    // Two main slopes (roof-coloured). Both face up, whatever handedness the OBB
    // basis came out with (see `tri_facing`).
    buf.quad_facing(e00, e10, r1, r0, roof, Vec3::Y);
    buf.quad_facing(e11, e01, r0, r1, roof, Vec3::Y);

    if hipped {
        // Hip ends are sloped roof triangles — they face up like the main slopes.
        buf.tri_facing(e00, r0, e01, roof, Vec3::Y);
        buf.tri_facing(e10, e11, r1, roof, Vec3::Y);
    } else {
        // Gable ends are vertical triangles (wall-coloured), so `Vec3::Y` can't
        // orient them — they face outward along the ridge axis instead. `a0 < a1`
        // are projections onto `a_ax`, so the `a0` end faces −`a_ax`.
        let a_out = Vec3::new(a_ax.x, 0.0, a_ax.y);
        buf.tri_facing(e00, r0, e01, wall, -a_out);
        buf.tri_facing(e10, e11, r1, wall, a_out);
    }
}

// --- Overpass fetch + parse (runs on the background thread) ---

/// On-disk cache path for a fixed CACHE_ZOOM cell (independent of patch size, so
/// changing the patch radius never invalidates the cache).
fn cell_cache_key(cx: i64, cy: i64, zoom: u32) -> String {
    format!("buildings/{}/{}/{}.json", zoom, cx, cy)
}

/// Assemble a patch's buildings from the fixed CACHE_ZOOM cells covering its bbox:
/// each cell is served from disk cache or fetched once, then deduped by OSM way id
/// (a building straddling a cell edge is returned by both cells).
/// The network half: every covering cell's response text, nothing parsed.
async fn fetch_patch_texts(south: f64, west: f64, north: f64, east: f64) -> Vec<String> {
    // NW corner → min tile (x,y); SE corner → max tile (y grows southward).
    let (cx0, cy0) = lat_lng_to_tile(north, west, CACHE_ZOOM);
    let (cx1, cy1) = lat_lng_to_tile(south, east, CACHE_ZOOM);

    let mut texts = Vec::new();
    for cy in cy0..=cy1 {
        for cx in cx0..=cx1 {
            match fetch_cell_text(cx, cy, CACHE_ZOOM).await {
                Ok(text) => texts.push(text),
                Err(e) => tracing::warn!(cx, cy, error = %e, "building cell fetch failed"),
            }
        }
    }
    texts
}

/// The CPU half: parse each cell, dedupe by OSM way id (a building straddling a
/// cell edge is returned by both cells), and clip to the requested bbox.
fn parse_patch(texts: &[String], south: f64, west: f64, north: f64, east: f64) -> Vec<Building> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for text in texts {
        match parse_buildings(text) {
            Ok(cell) => {
                for b in cell {
                    if seen.insert(b.id) {
                        out.push(b);
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "building cell parse failed"),
        }
    }
    // The cache cells are coarser than the patch; keep only buildings whose first
    // vertex falls in the requested bbox, so we spawn the patch, not whole cells.
    out.retain(|b| {
        let (lat, lng) = b.ring[0];
        lat >= south && lat <= north && lng >= west && lng <= east
    });
    out
}

/// One fixed cell's buildings. Covered cells (ADR-027) come from the self-hosted
/// 3DBAG host (real Dutch BAG heights); everything else — and any 3DBAG miss (a
/// 404 or transport error) — falls back to Overpass/OSM. Both paths return the
/// same `Building` shape via `parse_buildings`.
async fn fetch_cell_text(cx: i64, cy: i64, zoom: u32) -> Result<String, String> {
    if zoom == CACHE_ZOOM && tdbag_covers(cx, cy) {
        match fetch_cell_3dbag_text(cx, cy).await {
            Ok(text) => return Ok(text),
            Err(e) => {
                tracing::debug!(cx, cy, error = %e, "3DBAG cell fetch failed; falling back to OSM");
            }
        }
    }
    fetch_cell_overpass_text(cx, cy, zoom).await
}

/// Overpass reports timeouts, rate limits and runtime errors as HTTP 200 with a
/// `remark` field, so a response has to be checked before it is cached or it is
/// remembered as a permanently-empty cell.
///
/// The check deserialises *only* that field. `parse_buildings` would answer the
/// same question, but it builds a `serde_json::Value` tree of the entire response
/// to do it, and that tree — specifically dropping it — was the single largest
/// entry in the web build's main-thread profile. Serde skips `elements` here
/// without allocating, which keeps validate-before-persist on the network half
/// while the real parse moves to a worker.
#[derive(serde::Deserialize)]
struct OverpassRemark {
    #[serde(default)]
    remark: Option<String>,
}

fn overpass_remark(text: &str) -> Option<String> {
    serde_json::from_str::<OverpassRemark>(text).ok()?.remark
}

/// On-disk cache path for a 3DBAG cell — namespaced apart from the Overpass cache
/// (`cache/buildings/3dbag/…`) so a cell whose source flips never serves stale
/// geometry cached from the other source.
fn cell_cache_key_3dbag(cx: i64, cy: i64) -> String {
    format!("buildings/3dbag/{}/{}/{}.json", CACHE_ZOOM, cx, cy)
}

/// Fetch one cell from the 3DBAG static host (cached via the storage seam). The
/// host serves Overpass-shaped JSON, so `parse_buildings` handles it unchanged. A
/// 404 (cell not actually covered) or any transport error is returned as `Err`,
/// letting the caller fall back to Overpass.
async fn fetch_cell_3dbag_text(cx: i64, cy: i64) -> Result<String, String> {
    let key = cell_cache_key_3dbag(cx, cy);
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        return Ok(cached);
    }
    let url = format!(
        "{}/buildings/{}/{}/{}.json",
        tdbag_url(),
        CACHE_ZOOM,
        cx,
        cy
    );
    let bytes = http::fetch(http::Request::get(url)).await?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    // No validating parse here, unlike the Overpass path: this is our own static
    // host with no 200-status error channel to guard against, and a truncated
    // cache entry is caught when the worker fails to parse it (the caller then
    // evicts the key). Parsing on this thread is the cost being removed.
    storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
    Ok(text)
}

/// One fixed cell's buildings from disk cache or a single Overpass query (cached).
/// Cache hits skip the network (and the throttle) entirely.
async fn fetch_cell_overpass_text(cx: i64, cy: i64, zoom: u32) -> Result<String, String> {
    let key = cell_cache_key(cx, cy, zoom);
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        // A stale `remark`/error body cached before validate-on-write existed is
        // still worth catching, and costs nothing to check. A truncated write is
        // not caught here any more — the worker's parse fails and the caller
        // evicts the key, which is the same outcome one frame later.
        match overpass_remark(&cached) {
            None => return Ok(cached),
            Some(remark) => {
                tracing::warn!(key, %remark, "re-fetching cached Overpass error body");
                storage::remove(storage::Namespace::Cache, &key).await;
            }
        }
    }

    let (north, west) = tile_to_lat_lng(cx, cy, zoom);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, zoom);
    let query = format!(
        "[out:json][timeout:25];way[\"building\"]({south},{west},{north},{east});out geom;"
    );
    let fetched = overpass_query(&query).await?;
    // Space out live queries against a public instance so a multi-cell patch
    // doesn't burst-trip its limit; zero (skipped) against our own host.
    // Native only: a blocking sleep on the pool thread (as the old worker did); the
    // wasm build skips it (it cannot block) and leans on cache + the far dispatcher.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (_, throttle) = overpass_pace();
        if !throttle.is_zero() {
            std::thread::sleep(throttle);
        }
    }
    // Validate *before* persisting: `parse_buildings` rejects a `remark` response
    // (Overpass's 200-status error channel), so a transient timeout is never cached
    // as a permanently-empty cell.
    if let Some(remark) = overpass_remark(&fetched) {
        return Err(format!("Overpass remark: {}", remark.trim()));
    }
    storage::put(storage::Namespace::Cache, &key, fetched.as_bytes()).await;
    Ok(fetched)
}

/// POST an Overpass query, retrying a couple of times with backoff on failure
/// (429/504 are common under load), then falling through to the public instance.
///
/// A `remark` body is counted as a failure here, not passed back as success:
/// Overpass answers a timeout, a runtime error and a rate-limit with **HTTP 200**
/// and a `remark` field, so the transport-level retry this function exists for saw
/// those as wins and returned on the first one. That also cost the fallback its
/// whole reason to exist — an overloaded primary answers 200-with-remark far more
/// often than it refuses a connection. The caller re-checks (`fetch_cell_overpass_text`,
/// `parse_buildings`) so a remark can still never be cached as an empty cell.
pub(crate) async fn overpass_query(query: &str) -> Result<String, String> {
    let body = format!("data={}", crate::systems::geo_nav::percent_encode(query)).into_bytes();
    let mut last_err = String::new();
    for (endpoint, (url, attempts)) in overpass_endpoints().into_iter().enumerate() {
        if endpoint > 0 {
            tracing::warn!(%url, %last_err, "Overpass primary exhausted; trying fallback");
        }
        for attempt in 0..attempts {
            if attempt > 0 {
                // Backoff between attempts. Native only: a blocking sleep on the pool
                // thread; the wasm build cannot block, so it retries immediately (cache
                // + caller backoff cover the difference until an async timer lands).
                #[cfg(not(target_arch = "wasm32"))]
                std::thread::sleep(Duration::from_secs(3));
            }
            if endpoint > 0 {
                #[cfg(not(target_arch = "wasm32"))]
                pace_fallback();
            }
            let req = http::Request::post(url.as_str())
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body.clone());
            match http::fetch_string(req).await {
                Ok(text) => match overpass_remark(&text) {
                    None => return Ok(text),
                    Some(remark) => last_err = format!("Overpass remark: {}", remark.trim()),
                },
                Err(e) => last_err = e,
            }
        }
    }
    Err(last_err)
}

/// Parse an Overpass `out geom` response into buildings.
fn parse_buildings(text: &str) -> Result<Vec<Building>, String> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    // Overpass reports timeouts / runtime errors / rate limits as HTTP 200 with a
    // `remark` field (usually alongside empty `elements`). Treat it as an error so
    // it neither renders as "no buildings here" nor gets cached as a real result.
    if let Some(remark) = json.get("remark").and_then(|r| r.as_str()) {
        return Err(format!("Overpass remark: {}", remark.trim()));
    }
    let elements = json
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| "no elements in Overpass response".to_string())?;

    let mut out = Vec::new();
    for el in elements {
        if el.get("type").and_then(|t| t.as_str()) != Some("way") {
            continue;
        }
        let Some(geom) = el.get("geometry").and_then(|g| g.as_array()) else {
            continue;
        };
        let ring: Vec<(f64, f64)> = geom
            .iter()
            .filter_map(|pt| Some((pt.get("lat")?.as_f64()?, pt.get("lon")?.as_f64()?)))
            .collect();
        if ring.len() < 3 {
            continue;
        }
        let id = el.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let tags = el.get("tags");
        let seed = color_seed(ring[0]);
        let (height_m, height_is_eaves) = building_height(tags, seed);
        let (wall_color, roof_color) = derive_colors(tags, ring[0]);
        let mut roof_shape = parse_roof_shape(tag_str(tags, "roof:shape"));
        // roof:shape is sparsely tagged; small residential buildings are almost
        // always gabled, so default them to that (odd shapes flatten downstream).
        if roof_shape == RoofShape::Flat && default_pitched(tags, &ring) {
            roof_shape = RoofShape::Gabled;
        }
        let roof_height = tags
            .and_then(|t| t.get("roof:height"))
            .and_then(num_from_value);
        let roof_across = tag_str(tags, "roof:orientation") == Some("across");
        out.push(Building {
            id,
            ring,
            height_m,
            height_is_eaves,
            wall_color,
            roof_color,
            roof_shape,
            roof_height,
            roof_across,
        });
    }
    Ok(out)
}

/// Derive wall + roof colours from OSM tags, with heuristic fallbacks plus a small
/// deterministic per-building jitter so same-type buildings aren't identical.
/// Wall: `building:colour` → `building:material` → `building=<type>` → palette.
/// Roof: `roof:colour` → `roof:material` → type-based tiled/flat default.
fn derive_colors(tags: Option<&serde_json::Value>, ring0: (f64, f64)) -> ([f32; 3], [f32; 3]) {
    let seed = color_seed(ring0);
    let btype = tag_str(tags, "building").unwrap_or("yes");

    let wall_base = tag_color(
        tags,
        &["building:colour", "building:color", "colour", "color"],
    )
    .or_else(|| tag_str(tags, "building:material").and_then(material_color))
    .or_else(|| type_color(btype))
    .unwrap_or_else(|| palette_color(ring0));
    let wall = jitter(wall_base, seed, 0.12);

    let roof_base = tag_color(tags, &["roof:colour", "roof:color"])
        .or_else(|| tag_str(tags, "roof:material").and_then(roof_material_color))
        .unwrap_or_else(|| default_roof(btype, seed));
    let roof = jitter(roof_base, seed.rotate_left(17), 0.12);
    (wall, roof)
}

/// Default roof tone for a building type without `roof:*` tags: flat grey for
/// commercial/industrial, a varied tile colour otherwise (mostly residential).
fn default_roof(btype: &str, seed: u64) -> [f32; 3] {
    match btype.to_ascii_lowercase().as_str() {
        "commercial" | "retail" | "office" | "supermarket" | "industrial" | "warehouse"
        | "manufacture" | "factory" | "garage" | "garages" | "carport" | "roof" => {
            [0.52, 0.52, 0.54]
        }
        _ => ROOF_COLORS[(seed as usize) % ROOF_COLORS.len()],
    }
}

/// Deterministic seed from a footprint location.
fn color_seed(ring0: (f64, f64)) -> u64 {
    let (lat, lng) = ring0;
    (((lat * 1.0e5) as i64).wrapping_mul(73_856_093)
        ^ ((lng * 1.0e5) as i64).wrapping_mul(19_349_663)) as u64
}

/// Deterministic pseudo-random value in `[0, 1)` from a seed (SplitMix64).
fn unit_rand(seed: u64) -> f32 {
    let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x % 10_000) as f32 / 10_000.0
}

/// Nudge a colour's brightness by ±`amount` deterministically (weathering/variety).
fn jitter(c: [f32; 3], seed: u64, amount: f32) -> [f32; 3] {
    let f = 1.0 + (unit_rand(seed) - 0.5) * 2.0 * amount;
    [
        (c[0] * f).clamp(0.0, 1.0),
        (c[1] * f).clamp(0.0, 1.0),
        (c[2] * f).clamp(0.0, 1.0),
    ]
}

fn tag_str<'a>(tags: Option<&'a serde_json::Value>, key: &str) -> Option<&'a str> {
    tags?.get(key)?.as_str()
}

fn tag_color(tags: Option<&serde_json::Value>, keys: &[&str]) -> Option<[f32; 3]> {
    keys.iter()
        .find_map(|k| tag_str(tags, k).and_then(parse_color))
}

/// Parse an OSM colour value: `#RRGGBB` hex or a common named colour.
fn parse_color(s: &str) -> Option<[f32; 3]> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]);
        }
        return None;
    }
    Some(match s.to_ascii_lowercase().as_str() {
        "white" => [0.92, 0.92, 0.90],
        "black" => [0.18, 0.18, 0.18],
        "grey" | "gray" | "silver" => [0.60, 0.60, 0.60],
        "red" | "maroon" => [0.70, 0.25, 0.22],
        "brown" => [0.55, 0.42, 0.32],
        "beige" | "cream" | "ivory" => [0.85, 0.80, 0.70],
        "yellow" | "gold" => [0.85, 0.78, 0.45],
        "orange" => [0.80, 0.55, 0.35],
        "green" => [0.45, 0.55, 0.40],
        "blue" => [0.45, 0.55, 0.68],
        "sandstone" | "tan" | "sand" => [0.80, 0.72, 0.60],
        "pink" => [0.85, 0.68, 0.68],
        _ => return None,
    })
}

/// Wall colour by `building:material`.
fn material_color(s: &str) -> Option<[f32; 3]> {
    Some(match s.to_ascii_lowercase().as_str() {
        "brick" | "red_brick" => [0.70, 0.42, 0.35],
        "concrete" | "cement" | "reinforced_concrete" => [0.72, 0.72, 0.70],
        "glass" => [0.55, 0.68, 0.75],
        "wood" | "timber" | "timber_framing" => [0.62, 0.48, 0.35],
        "stone" | "limestone" | "sandstone" | "granite" => [0.78, 0.74, 0.66],
        "plaster" | "render" | "stucco" | "plastered" => [0.86, 0.82, 0.76],
        "metal" | "steel" => [0.66, 0.68, 0.70],
        _ => return None,
    })
}

/// Wall colour by `building=<type>`.
fn type_color(s: &str) -> Option<[f32; 3]> {
    Some(match s.to_ascii_lowercase().as_str() {
        "house" | "detached" | "residential" | "apartments" | "terrace" | "semidetached_house" => {
            [0.80, 0.74, 0.66]
        }
        "commercial" | "retail" | "office" | "supermarket" => [0.73, 0.73, 0.73],
        "industrial" | "warehouse" | "manufacture" | "factory" => [0.66, 0.66, 0.62],
        "church" | "cathedral" | "chapel" | "temple" | "mosque" | "synagogue" => [0.80, 0.76, 0.66],
        "garage" | "garages" | "shed" | "hut" | "roof" | "carport" => [0.70, 0.68, 0.64],
        _ => return None,
    })
}

/// Roof colour by `roof:material`.
fn roof_material_color(s: &str) -> Option<[f32; 3]> {
    Some(match s.to_ascii_lowercase().as_str() {
        "tile" | "tiles" | "roof_tiles" | "clay" | "clay_tiles" => [0.66, 0.34, 0.27],
        "metal" | "tin" | "zinc" | "copper" | "steel" => [0.55, 0.57, 0.60],
        "concrete" => [0.62, 0.62, 0.60],
        "slate" => [0.36, 0.38, 0.42],
        "tar_paper" | "asphalt" | "bitumen" | "gravel" => [0.30, 0.30, 0.32],
        "glass" => [0.55, 0.68, 0.75],
        "eternit" | "asbestos" => [0.60, 0.60, 0.58],
        "grass" | "green" => [0.45, 0.55, 0.38],
        _ => return None,
    })
}

/// Deterministic fallback wall tone from the footprint location, so untagged
/// buildings still read as distinct rather than a uniform mass.
fn palette_color(ring0: (f64, f64)) -> [f32; 3] {
    BUILDING_COLORS[(color_seed(ring0) as usize) % BUILDING_COLORS.len()]
}

/// Whether an untagged building should default to a pitched (gabled) roof: small
/// residential footprints, which are overwhelmingly pitched in European towns.
fn default_pitched(tags: Option<&serde_json::Value>, ring: &[(f64, f64)]) -> bool {
    let btype = tag_str(tags, "building").unwrap_or("yes");
    let residential = matches!(
        btype.to_ascii_lowercase().as_str(),
        "house"
            | "detached"
            | "semidetached_house"
            | "terrace"
            | "residential"
            | "yes"
            | "bungalow"
            | "cabin"
            | "farm"
    );
    if !residential {
        return false;
    }
    // Approximate footprint extent in metres (equirectangular near the anchor).
    let lat0 = ring[0].0;
    let mlng = 111_320.0 * lat0.to_radians().cos();
    let (mut minx, mut maxx, mut miny, mut maxy) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &(lat, lng) in ring {
        let (x, y) = (lng * mlng, lat * 111_320.0);
        minx = minx.min(x);
        maxx = maxx.max(x);
        miny = miny.min(y);
        maxy = maxy.max(y);
    }
    let (w, h) = (maxx - minx, maxy - miny);
    w.min(h) < 14.0 && (w * h) < 400.0
}

/// Map an OSM `roof:shape` value to a form we can build (unknown → flat).
fn parse_roof_shape(s: Option<&str>) -> RoofShape {
    match s {
        Some("gabled") => RoofShape::Gabled,
        Some("hipped") | Some("half-hipped") => RoofShape::Hipped,
        Some("pyramidal") | Some("pyramid") => RoofShape::Pyramidal,
        Some("skillion") | Some("lean_to") | Some("mono_pitch") => RoofShape::Skillion,
        _ => RoofShape::Flat,
    }
}

/// Building height and whether it's an eaves (wall-top) height. `height` → total;
/// `building:levels` → eaves; otherwise a type-based estimate (with jitter) → eaves.
fn building_height(tags: Option<&serde_json::Value>, seed: u64) -> (f32, bool) {
    if let Some(h) = tags.and_then(|t| t.get("height")).and_then(num_from_value) {
        return (h.max(2.0), false);
    }
    if let Some(levels) = tags
        .and_then(|t| t.get("building:levels"))
        .and_then(num_from_value)
    {
        return ((levels * STOREY_HEIGHT).max(2.5), true);
    }
    let btype = tag_str(tags, "building").unwrap_or("yes");
    (estimate_height(btype, seed), true)
}

/// Estimate an untagged building's eaves height from its type, with a deterministic
/// per-building jitter so neighbours vary (no flat, uniform blocks).
fn estimate_height(btype: &str, seed: u64) -> f32 {
    let (base, spread) = match btype.to_ascii_lowercase().as_str() {
        "house" | "detached" | "bungalow" | "cabin" => (6.0, 1.5),
        "semidetached_house" | "terrace" => (7.5, 1.5),
        "residential" | "farm" => (8.0, 2.0),
        "apartments" | "dormitory" => (12.0, 4.0),
        "commercial" | "retail" | "office" | "supermarket" => (9.0, 3.0),
        "industrial" | "warehouse" | "manufacture" | "factory" => (7.0, 2.5),
        "church" | "cathedral" | "chapel" | "temple" | "mosque" => (14.0, 4.0),
        "garage" | "garages" | "shed" | "hut" | "roof" | "carport" => (3.0, 0.8),
        _ => (7.0, 2.5),
    };
    (base + (unit_rand(seed.rotate_left(7)) * 2.0 - 1.0) * spread).max(2.5)
}

/// Parse a leading number from a JSON value that may be a number or a string like "12 m".
fn num_from_value(v: &serde_json::Value) -> Option<f32> {
    if let Some(n) = v.as_f64() {
        return Some(n as f32);
    }
    let s = v.as_str()?;
    let num: String = s
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    num.parse::<f32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every triangle of a closed building solid must be wound so its face normal
    /// points *away* from the solid's interior — that invariant is what makes
    /// `BACKFACE_CULL` safe. Source rings come from OSM in either orientation and
    /// earcut preserves whichever it got, so this is checked rather than assumed.
    ///
    /// Uses the centroid as the reference interior point, which is valid because
    /// the footprints below are convex and the solid is star-shaped about it.
    fn assert_outward(buf: &MeshData, label: &str) {
        assert!(!buf.indices.is_empty(), "{label}: produced no geometry");
        let pos = |i: u32| {
            let p = buf.positions[i as usize];
            Vec3::new(p[0], p[1], p[2])
        };
        let interior = {
            let sum: Vec3 = buf.indices.iter().map(|&i| pos(i)).sum();
            sum / buf.indices.len() as f32
        };
        for (t, tri) in buf.indices.chunks_exact(3).enumerate() {
            let (a, b, c) = (pos(tri[0]), pos(tri[1]), pos(tri[2]));
            let normal = (b - a).cross(c - a);
            if normal.length_squared() < 1e-12 {
                continue; // degenerate sliver: no facing to speak of
            }
            let outward = (a + b + c) / 3.0 - interior;
            assert!(
                normal.dot(outward) > 0.0,
                "{label}: triangle {t} is wound inward (normal {normal:?}, outward {outward:?})",
            );
        }
    }

    /// A square footprint, given in both winding orders: OSM tags don't guarantee
    /// one, and the fix must not depend on it.
    fn square(clockwise: bool) -> Vec<Vec2> {
        let mut r = vec![
            Vec2::new(-10.0, -10.0),
            Vec2::new(10.0, -10.0),
            Vec2::new(10.0, 10.0),
            Vec2::new(-10.0, 10.0),
        ];
        if clockwise {
            r.reverse();
        }
        r
    }

    /// The far ring must reach the LoD2.2 handover, on every tier.
    ///
    /// `lod22` draws real meshes out to `lod22_radius()`; from `lod22_radius() + 1`
    /// outward these simplified extrusions take over. If the far ring were the
    /// shorter of the two, the cells in between would be owned by neither streamer
    /// and their buildings would simply be absent — at a distance that moves with
    /// the tier, which is the kind of gap that gets reported as "buildings missing
    /// on the phone" and reproduces nowhere else.
    ///
    /// Both radii take the same `radius_bias`, so this holds by construction today;
    /// the test is here because the two are adjusted independently and nothing else
    /// would notice them diverging.
    #[test]
    fn the_far_ring_meets_the_lod22_handover() {
        let handover = crate::systems::lod22::lod22_radius() + 1;
        assert!(
            far_cell_radius() >= handover,
            "far cells reach {} but LoD2.2 hands over at {handover}: the ring \
             between them would have no buildings at all",
            far_cell_radius(),
        );
    }

    /// A fresh run must see **no** toggle edge.
    ///
    /// `OsmBuildings::enabled_was` and `BUILDINGS_USER_ENABLED` are two copies of
    /// one fact, and the off-edge they drive tears down the whole patch. If they
    /// disagreed at startup, the first frame would retire a ring it had not built
    /// and clear colliders nobody published -- harmless once, but it would fire on
    /// every cold start and mask a real toggle edge in the noise.
    #[test]
    fn the_buildings_toggle_and_its_edge_state_agree_at_startup() {
        assert!(
            buildings_user_enabled(),
            "the master Buildings toggle starts on, matching DataLayers::default"
        );
        assert!(
            OsmBuildings::default().enabled_was,
            "the streamer must start believing the toggle is on, or frame one is a teardown"
        );
    }

    #[test]
    fn detail_buildings_are_wound_outward() {
        let shapes = [
            (RoofShape::Flat, "flat"),
            (RoofShape::Gabled, "gabled"),
            (RoofShape::Hipped, "hipped"),
            (RoofShape::Pyramidal, "pyramidal"),
            (RoofShape::Skillion, "skillion"),
        ];
        for clockwise in [false, true] {
            for (shape, name) in shapes {
                let mut bld = MeshData::with_dedup();
                let mut wins: [MeshData; 4] = std::array::from_fn(|_| MeshData::default());
                push_building(
                    &mut bld,
                    &mut wins,
                    1,
                    &square(clockwise),
                    20.0,
                    true,
                    [0.5, 0.5, 0.5],
                    [0.4, 0.3, 0.3],
                    shape,
                    None,
                    false,
                );
                assert_outward(&bld, &format!("{name} (clockwise={clockwise})"));
            }
        }
    }

    #[test]
    fn far_lod_buildings_are_wound_outward() {
        for clockwise in [false, true] {
            let mut bld = MeshData::with_dedup();
            let mut wins: [MeshData; 4] = std::array::from_fn(|_| MeshData::default());
            push_building_lod(
                &mut bld,
                &mut wins,
                1,
                &square(clockwise),
                20.0,
                [0.5, 0.5, 0.5],
                [0.4, 0.3, 0.3],
            );
            assert_outward(&bld, &format!("far lod (clockwise={clockwise})"));
        }
    }

    /// Swapping SipHash for [`VertexHasher`] cannot make the mesh wrong —
    /// `HashMap` still compares keys — but a hash that clusters would quietly
    /// turn the dedup into a linear scan, and nothing else would show it. So
    /// check the property that actually matters: over the kind of keys real
    /// geometry produces (a lattice of positions sharing a handful of normals
    /// and colours), the top bits that hashbrown buckets on stay spread out.
    #[test]
    fn vertex_hasher_spreads_realistic_keys() {
        use std::hash::{BuildHasher, BuildHasherDefault};

        let build: BuildHasherDefault<VertexHasher> = BuildHasherDefault::default();
        let mut keys = Vec::new();
        for x in 0..40u32 {
            for z in 0..40u32 {
                for up in [0.0f32, 1.0] {
                    keys.push([
                        (x as f32 * 2.5).to_bits(),
                        0.0f32.to_bits(),
                        (z as f32 * 2.5).to_bits(),
                        0.0f32.to_bits(),
                        up.to_bits(),
                        (1.0f32 - up).to_bits(),
                        0.5f32.to_bits(),
                        0.5f32.to_bits(),
                        0.5f32.to_bits(),
                        1.0f32.to_bits(),
                    ]);
                }
            }
        }

        let hashes: Vec<u64> = keys.iter().map(|k| build.hash_one(k)).collect();
        let distinct: std::collections::HashSet<u64> = hashes.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            keys.len(),
            "distinct keys must hash distinctly"
        );

        // hashbrown buckets on the low bits and tags on the high seven. A hash
        // that ignored either would pile every key into a few buckets.
        let mut buckets = [0usize; 128];
        for h in &hashes {
            buckets[(h % 128) as usize] += 1;
        }
        let worst = buckets.iter().copied().max().unwrap_or(0);
        let ideal = keys.len() / buckets.len();
        assert!(
            worst < ideal * 4,
            "worst bucket {worst} vs ideal {ideal} — the hash is clustering"
        );
    }

    /// Dedup must only merge vertices that agree in position, normal *and* colour,
    /// so a cube's eight corners stay eight-per-face-triple, not eight total.
    #[test]
    fn dedup_preserves_normal_and_colour_discontinuities() {
        let mut bld = MeshData::with_dedup();
        let mut wins: [MeshData; 4] = std::array::from_fn(|_| MeshData::default());
        push_building_lod(
            &mut bld,
            &mut wins,
            1,
            &square(false),
            20.0,
            [0.5, 0.5, 0.5],
            [0.4, 0.3, 0.3],
        );
        // 4 walls × 4 corners (each wall has its own normal, so no sharing across
        // the vertical creases) + a flat cap that shares its 4 ring corners.
        assert_eq!(bld.positions.len(), 20);
        assert_eq!(bld.positions.len(), bld.normals.len());
        assert_eq!(bld.positions.len(), bld.colors.len());
    }
}
