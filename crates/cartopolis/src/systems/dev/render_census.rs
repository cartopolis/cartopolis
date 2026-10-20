//! What the renderer was actually asked to draw: draw calls and triangles, split
//! by the streamer that produced them.
//!
//! One walk of every `Mesh3d`, two readers. [`log_render_cost`] writes it to
//! a periodic log line; `shot_harness::drive_capture` writes it into
//! [`crate::systems::shot_harness::ShotMetrics`], where it reaches the `shot state`
//! line, `--csv`, `--dump-state` and `--expect` at once. It used to exist only in
//! the first of those, which meant the numbers the whole phone budget is a set of
//! predictions about were computed every five seconds and thrown away.
//!
//! **These are the metrics a CPU rasteriser can still be trusted on.** Colour,
//! exposure and lighting are not evidence under lavapipe
//! (`docs/notes/headless-shots-software-renderer.md`), but a draw count is a
//! property of what the client *selected*, not of what rasterised it: culling, LOD
//! and visibility run the same code on any adapter. So these are the columns a CI
//! gate may assert, and the timing columns beside them are not.

use bevy::camera::visibility::ViewVisibility;
use bevy::ecs::system::SystemParam;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use bevy::render::mesh::Mesh3d;

use crate::components::{GlobeMesh, OsmBuildingMesh};
use crate::systems::lod22::Lod22Mesh;
use crate::systems::map_stream::StreamTile;

/// One frame's submitted geometry. Triangles are counted per *source* so a
/// regression names the streamer that caused it rather than just the total.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderCounts {
    /// Entities with a visible mesh — one CPU-issued draw each on the mobile
    /// GPUs that Bevy excludes from GPU preprocessing (Adreno ≤ 730, older Mali),
    /// which is why the count matters on its own and not only the triangles.
    pub draws: usize,
    pub tris_total: usize,
    /// Triangles from meshes that cast shadows, i.e. what each cascade re-draws.
    pub tris_shadow: usize,
    /// *Entities* that cast, which is not the same question as `tris_shadow` and
    /// is the only one some of the casters answer at all.
    ///
    /// A mesh uploaded `RENDER_WORLD`-only counts 0 triangles here by
    /// construction (see [`Self::triangles`]), and the clipmap tiles are exactly
    /// that — so when the whole tile pyramid was being submitted to every cascade
    /// of every light, unculled (fixed 2026-09-11; they carry `NotShadowCaster`
    /// from spawn now), `tris_shadow` reported the same number before and after.
    /// The cost of a caster is a draw and a pass over its fill whatever the main
    /// world remembers of its geometry, so count them too.
    pub shadow_casters: usize,
    pub tris_buildings: usize,
    pub tris_lod22: usize,
    pub tris_tiles: usize,
    pub tris_globe: usize,
}

/// Resident mesh bytes, split as [`RenderCensus::mesh_bytes`] splits them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshBytes {
    pub lod22: usize,
    pub buildings: usize,
    pub tiles: usize,
    pub globe: usize,
    pub other: usize,
    pub unowned: usize,
}

/// The queries behind [`RenderCounts`], as a `SystemParam` so both readers share
/// one definition — and so `drive_capture`, which is at Bevy's 16-parameter
/// ceiling, can take it as a single argument.
#[derive(SystemParam)]
pub struct RenderCensus<'w, 's> {
    meshes: Res<'w, Assets<Mesh>>,
    drawn: Query<
        'w,
        's,
        (
            &'static Mesh3d,
            &'static ViewVisibility,
            Has<NotShadowCaster>,
            Has<OsmBuildingMesh>,
            Has<Lod22Mesh>,
            Has<StreamTile>,
            Has<GlobeMesh>,
        ),
    >,
}

impl RenderCensus<'_, '_> {
    /// Walk every mesh entity and total what the view drew.
    ///
    /// This resolves every handle into `Assets<Mesh>`, so it is a real hitch on a
    /// dense scene — cheap enough once per capture or once per five seconds, not
    /// cheap enough per frame. Both callers throttle it; neither should stop.
    pub fn counts(&self) -> RenderCounts {
        let mut c = RenderCounts::default();
        for (mesh, visibility, no_shadow, building, lod22, tile, globe) in &self.drawn {
            if !visibility.get() {
                continue; // built, but not drawn this frame
            }
            let tris = self.triangles(&mesh.0);
            c.draws += 1;
            c.tris_total += tris;
            if !no_shadow {
                c.tris_shadow += tris;
                c.shadow_casters += 1;
            }
            if building {
                c.tris_buildings += tris;
            }
            if lod22 {
                c.tris_lod22 += tris;
            }
            if tile {
                c.tris_tiles += tris;
            }
            if globe {
                c.tris_globe += tris;
            }
        }
        c
    }

    /// Resident mesh bytes by the streamer whose entities hold them, drawn this
    /// frame or not, each mesh counted once however many entities share it.
    /// `unowned` is held by no entity at all: the retained-mesh pool
    /// (`quality::warm_gpu_ceiling`), or a leak.
    pub fn mesh_bytes(&self, meter: &crate::systems::upload_meter::UploadMeter) -> MeshBytes {
        let mut seen = bevy::platform::collections::HashSet::new();
        let mut b = MeshBytes::default();
        for (mesh, _, _, building, lod22, tile, globe) in &self.drawn {
            if !seen.insert(mesh.0.id()) {
                continue;
            }
            let Some(bytes) = meter.mesh_bytes(mesh.0.id()) else {
                continue;
            };
            let slot = if lod22 {
                &mut b.lod22
            } else if building {
                &mut b.buildings
            } else if tile {
                &mut b.tiles
            } else if globe {
                &mut b.globe
            } else {
                &mut b.other
            };
            *slot += bytes;
        }
        let owned = b.lod22 + b.buildings + b.tiles + b.globe + b.other;
        b.unowned = meter.mesh_bytes_total().saturating_sub(owned);
        b
    }

    /// Triangles in one mesh, or 0 when the geometry is no longer in the main
    /// world.
    ///
    /// The non-panicking accessors are load-bearing: meshes uploaded
    /// `RENDER_WORLD`-only (tiles, globe, transit) have had their main-world
    /// geometry dropped after extraction, and `indices()`/`count_vertices()`
    /// assert `MAIN_WORLD`. Counting those as 0 under-reports the tile tiers; it
    /// is the honest answer available from this side and it does not crash.
    fn triangles(&self, handle: &Handle<Mesh>) -> usize {
        self.meshes.get(handle).map_or(0, |mesh| {
            match mesh.try_indices_option() {
                Ok(Some(indices)) => indices.len() / 3,
                Ok(None) => mesh.count_vertices() / 3, // non-indexed, still main world
                Err(_) => 0,                           // extracted to the render world
            }
        })
    }
}

/// The periodic half of [`systems::render_census`] — the same walk the shot
/// harness makes per capture, on a five-second timer.
///
/// Both the walk and the phone gate are deliberate. Resolving every `Mesh3d`
/// handle into `Assets<Mesh>` is a real hitch on a dense scene, so it is throttled
/// rather than run per frame; and the gate is a `run_if` at registration rather
/// than an early return here, because a return does not save the query — Bevy
/// still matches the census's seven-component `Query` against every archetype the
/// streamers create.
///
/// The phone *does* get it on request, which is the part that changed. Gating it
/// off entirely meant the numbers the phone budget is a set of predictions about
/// (`docs/notes/wasm-performance.md`) were the ones the phone could never report,
/// and a scripted capture cannot stand in — Android has no CLI. `CARTO_RENDER_COST`
/// goes through `env_flag`, so on a device it is `adb shell setprop
/// debug.carto.render_cost 1` against the shipped APK, no rebuild.
pub fn log_render_cost(
    time: Res<Time<Real>>,
    mut next: Local<f32>,
    census: RenderCensus,
    meter: Res<crate::systems::upload_meter::UploadMeter>,
) {
    *next -= time.delta_secs();
    if *next > 0.0 {
        return;
    }
    *next = 5.0;
    let c = census.counts();
    tracing::info!(
        draws = c.draws,
        ktris_total = c.tris_total / 1000,
        ktris_shadow = c.tris_shadow / 1000,
        shadow_casters = c.shadow_casters,
        ktris_buildings = c.tris_buildings / 1000,
        ktris_lod22 = c.tris_lod22 / 1000,
        ktris_tiles = c.tris_tiles / 1000,
        ktris_globe = c.tris_globe / 1000,
        "render cost",
    );
    // Where the mesh memory is: the line to read beside `gpu` when the GPU
    // fills (`upload_meter::GpuReport`).
    let mb = |b: usize| b / (1024 * 1024);
    let m = census.mesh_bytes(&meter);
    tracing::info!(
        lod22_mb = mb(m.lod22),
        buildings_mb = mb(m.buildings),
        tiles_mb = mb(m.tiles),
        globe_mb = mb(m.globe),
        other_mb = mb(m.other),
        unowned_mb = mb(m.unowned),
        "mesh memory",
    );
}

/// The `other` column of [`RenderCensus::mesh_bytes`], named: resident mesh bytes
/// of every mesh entity no streamer marker covers, grouped by the first of this
/// crate's components the entity carries. Every five seconds, top six, beside
/// `mesh memory`.
///
/// Introspection rather than a marker per streamer, because a marker answers only
/// the question someone thought to ask: it was a zoom-out putting 1.3 GB of meshes
/// in this bucket (2026-10-20) that made it worth naming.
#[allow(clippy::type_complexity)]
pub fn log_mesh_owners(
    world: &World,
    time: Res<Time<Real>>,
    mut next: Local<f32>,
    meter: Res<crate::systems::upload_meter::UploadMeter>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: crate::grid::AnchoredCamera,
    meshes: Query<
        (Entity, &Mesh3d),
        (
            Without<OsmBuildingMesh>,
            Without<Lod22Mesh>,
            Without<StreamTile>,
            Without<GlobeMesh>,
        ),
    >,
) {
    *next -= time.delta_secs();
    if *next > 0.0 {
        return;
    }
    *next = 5.0;
    let mut seen = bevy::platform::collections::HashSet::new();
    // (bytes, distinct meshes)
    let mut by_owner: bevy::platform::collections::HashMap<String, (usize, usize)> =
        Default::default();
    for (entity, mesh) in &meshes {
        if !seen.insert(mesh.0.id()) {
            continue;
        }
        let Some(bytes) = meter.mesh_bytes(mesh.0.id()) else {
            continue;
        };
        // The entity, then its parents: a glTF scene's meshes carry only bevy's
        // components, and the app's marker is on the scene root above them.
        // A fixed list rather than component names, which a release build does
        // not keep (bevy's `debug` feature).
        let mut owner = "(none of the listed markers)";
        let mut at = Some(entity);
        for _ in 0..8 {
            let Some(e) = at else { break };
            let Ok(r) = world.get_entity(e) else { break };
            if let Some(name) = mesh_owner(&r) {
                owner = name;
                break;
            }
            at = r.get::<ChildOf>().map(ChildOf::parent);
        }
        let owner = owner.to_string();
        let slot = by_owner.entry(owner).or_default();
        slot.0 += bytes;
        slot.1 += 1;
    }
    let mut top: Vec<(String, (usize, usize))> = by_owner.into_iter().collect();
    top.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let line: Vec<String> = top
        .iter()
        .take(6)
        .map(|(name, (bytes, n))| format!("{name}={}MB/{n}", bytes / (1024 * 1024)))
        .collect();
    let (cells, ring_mb) = world
        .get_resource::<crate::systems::map_geometry::MapSurfaces>()
        .map_or((0, 0), |s| {
            (s.loaded_cells(), s.ring_bytes() / (1024 * 1024))
        });
    let altitude_m = camera
        .get()
        .map_or(f64::NAN, |(cell, tf)| globe.altitude_m(cell, tf));
    tracing::info!(
        owners = %line.join(" "),
        surface_cells = cells,
        surface_ring_mb = ring_mb,
        altitude_m = altitude_m as i64,
        "mesh owners",
    );
}

/// The marker [`log_mesh_owners`] files a mesh entity under, if it has one.
fn mesh_owner(e: &bevy::ecs::world::EntityRef) -> Option<&'static str> {
    use crate::components as c;
    use crate::systems as s;
    macro_rules! first {
        ($($t:ty => $name:literal),* $(,)?) => {
            $(if e.contains::<$t>() { return Some($name); })*
        };
    }
    first!(
        c::MapSurface => "MapSurface",
        c::TunnelSurface => "TunnelSurface",
        c::VegetationSurface => "VegetationSurface",
        c::FurnitureSurface => "FurnitureSurface",
        c::GroundSkirt => "GroundSkirt",
        c::MapBackstop => "MapBackstop",
        c::PrimMesh => "PrimMesh",
        c::StreetLabelSign => "StreetLabelSign",
        c::VehicleBox => "VehicleBox",
        c::RouteVisual => "RouteVisual",
        c::DataLayerMarkers => "DataLayerMarkers",
        c::PlanetAtmosphere => "PlanetAtmosphere",
        c::Avatar => "Avatar",
        c::RemoteAvatar => "RemoteAvatar",
        s::grass::GrassPatch => "GrassPatch",
        s::interiors::InteriorMesh => "InteriorMesh",
        s::street_detail::PropNear => "PropNear",
        s::street_detail::PropFar => "PropFar",
        s::sky_dome::SkyDome => "SkyDome",
        s::satellites::SatelliteCloud => "SatelliteCloud",
        s::traffic::Car => "Car",
        s::tall_structures::TurbineRotor => "TurbineRotor",
        s::sewers::Backdrop => "SewerBackdrop",
        c::HideInSpace => "HideInSpace (other)",
    );
    None
}
