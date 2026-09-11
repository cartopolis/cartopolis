//! What the renderer was actually asked to draw: draw calls and triangles, split
//! by the streamer that produced them.
//!
//! One walk of every `Mesh3d`, two readers. `lib.rs::log_render_cost` writes it to
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
