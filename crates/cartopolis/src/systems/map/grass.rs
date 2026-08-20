//! Near-field grass — the ECS half: the patch pool, the moving mask, the wind and
//! the altitude ramp.
//!
//! The pure half is [`crate::systems::turf`] and the geometry is
//! `assets/shaders/grass.wgsl`; both have the reasoning for *why* a blade is
//! generated in the vertex shader rather than scattered and uploaded. What this
//! module owns is the consequence of that decision, which is that grass does not
//! stream. There is:
//!
//! * **one mesh** — [`turf::blade_lattice`], uploaded at startup and never again;
//! * **one material** — rewritten when the wind changes (a few times an hour) and
//!   when the mask moves (every twenty-odd metres of walking);
//! * **a fixed pool of patch entities**, spawned once, which slide along a
//!   world-aligned lattice under the camera. A patch never spawns, never
//!   despawns, and never has its mesh rebuilt: crossing a lattice line moves a
//!   `Transform` and rewrites a `MeshTag`.
//!
//! That last point is the whole reason this layer exists in the shape it does.
//! Every other streamer in this tree is bounded per frame because mass mesh churn
//! loses the device (CLAUDE.md, `docs/notes/upload-bursts.md`), and grass at a
//! legible density is an order of magnitude more churn than anything else here —
//! *continuous* churn, for as long as the avatar is walking, which no per-frame
//! cap makes free. A pool that only moves has no churn to bound.
//!
//! # What the mask is for
//!
//! A blade needs two facts no hash can give it: whether this spot is grass, and
//! how high the ground under it is. Both ride in a small `Rg8Unorm` texture around
//! the camera ([`turf::mask_field`]) — red is coverage, green is the terrain lift
//! sampled from the same [`TerrainField`] the ground *mesh* was built from, so the
//! blades stand on the ground rather than through it.
//!
//! It is rebuilt on a worker, because it is ~150k point-in-polygon tests, and
//! double-buffered: the old mask keeps being sampled until the new one lands. The
//! rebuild trigger has [`FIELD_MARGIN_M`] of slack in it precisely so that is
//! never a race — the camera cannot outwalk the mask it is standing in.

use bevy::asset::RenderAssetUsages;
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::ViewVisibility;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::NotShadowCaster;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::mesh::MeshTag;
use bevy::render::render_resource::{AsBindGroup, Extent3d, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;
use big_space::prelude::{BigSpace, Grid, GridCell};
use std::sync::{Arc, Mutex};

use crate::components::{FreeCamera, HideInSpace};
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::globe::GlobeState;
use crate::systems::sky::CLOUD_WIND;
use crate::systems::terrain::TerrainField;
use crate::systems::turf::{self, GrassArea, MaskField, PlacedArea};
use crate::systems::vector_tiles::{palette, SurfaceGroup};
use crate::systems::weather::WeatherState;

// ─── The shape of a lawn ─────────────────────────────────────────────────────

/// Edge length of one patch, in metres, and the unit the shader's `MeshTag`
/// counts in.
///
/// Small enough that a patch is a meaningful unit of frustum culling — the pool
/// covers a disc and roughly half of it is behind you at any moment — and large
/// enough that the pool is a few dozen entities rather than a few hundred.
const PATCH_M: f32 = 8.0;

/// Blade slots along one edge of a patch, per quality tier. `slots()²` blades per
/// patch, so this and [`PATCH_M`] together are the density: 96² over 64 m² is
/// ~144 blades/m².
///
/// **Density is the one number that decides whether this reads as a lawn**, and
/// it took four captures over the Noorderplantsoen on 2026-08-20 to find: 9
/// blades/m² read as scattered weeds, 16 as a sparse field, 25 as tufts with bare
/// ground between them. Grass only stops looking like individual plants when the
/// blades overlap, and no amount of getting the blade *shape* right substitutes
/// for that. Real turf is thousands per m²; this is as close as a real-time
/// budget gets.
///
/// It costs nothing to *store* — the lattice is one shared mesh, ~2.5 MB — so
/// what density actually buys is paid in **vertex invocations**: ~46k per drawn
/// patch, and a pool with ~17 patches in frame is ~800k a frame. That is cheap on
/// a desktop GPU and is not free on a tiler, which is why the handset rung takes
/// both a lower count here *and* half the radius in [`pool_offsets`] — a quarter
/// of the area at a third of the density.
fn slots() -> u32 {
    if crate::quality::budget().detail_layers {
        112
    } else {
        64
    }
}

/// Size of a clump, in metres — the scale grass varies on because the soil under
/// it does. Blades in one clump share a lean, a vigour and a shade of green.
///
/// Chosen against blade height rather than by eye: a clump wants to be a few
/// blades across, so a tuft reads as a tuft and not as a lawn-sized patch of
/// colour. Much larger and the variation stops looking like grass and starts
/// looking like mowing stripes.
const CLUMP_M: f32 = 0.45;

/// What the thinnest and thickest patches keep, as a fraction of the slots
/// available to them.
///
/// The floor is not zero on purpose: bald patches in the middle of a lawn read as
/// a bug in the coverage mask, which is exactly the thing this layer's diagnostics
/// exist to distinguish. Thin, not absent.
const DENSITY_LOW: f32 = 0.34;
const DENSITY_HIGH: f32 = 1.0;

/// How far a blade's facing may swing off its clump's, in radians. Wide enough
/// that a tuft fans, narrow enough that it is recognisably one tuft.
const CLUMP_FAN: f32 = 2.0;

/// Blade height and width in metres, before the shader's per-blade jitter.
///
/// Mown grass, not a hayfield: the map this stands on is a pale cartographic
/// drawing, and knee-high meadow over it reads as a texture error rather than as
/// vegetation. Tall grass belongs to a `land` kind (`meadow`, `heath`) that the
/// coverage mask cannot currently distinguish, which is the obvious next step.
///
/// These are the numbers the first capture actually moved. 26 cm × 2.6 cm was
/// chosen from a table of "what is a blade of grass" and rendered as a bed of
/// reeds standing to the avatar's knee — the thing a still frame is good for
/// catching, and the thing no amount of reading the constant would have. A real
/// mown blade is ~5 cm and would be invisible past a metre; this is the
/// compromise, and it is deliberately on the tall side of a lawn because the
/// whole point of the layer is that you can see it.
const BLADE_HEIGHT_M: f32 = 0.15;
/// Narrower than a real blade looks from standing height, on purpose: at
/// [`slots`]'s density a wider blade closes into an opaque mat and the lawn loses
/// the depth the root shading exists to give it.
const BLADE_WIDTH_M: f32 = 0.010;

/// How far a blade may be jittered off its lattice slot, as a fraction of the
/// slot spacing. Just under half, so blades never swap places and the density
/// stays exactly what [`SLOTS`] says.
const JITTER_FRAC: f32 = 0.9;

/// Sway amplitude at [`FULL_SWAY_WIND`], as a fraction of blade height, and the
/// wind speed the ramp saturates at.
///
/// The same shape `water::sea_state` uses and for the same reason: a still lawn
/// is a real state but a rare one, and one that reads as "the shader isn't
/// running", so the calm end is a stir rather than nothing.
const CALM_SWAY: f32 = 0.10;
const FULL_SWAY: f32 = 0.55;
const FULL_SWAY_WIND: f32 = 12.0;

/// Wind assumed until a weather sample lands — a light breeze, matching
/// `water::UNKNOWN_WIND`, because the two are the same wind.
const UNKNOWN_WIND: f32 = 3.0;

/// Wind change (m/s) needed before the material is rewritten. The animation clock
/// is `globals.time` inside the shader, so this system tracks *weather*, which
/// moves on the scale of minutes.
const WIND_EPSILON: f32 = 0.25;

// ─── How far it reaches ──────────────────────────────────────────────────────

/// Radius of the patch pool, in metres, at the full quality tier — and with it
/// the distance at which the last blade has retired.
///
/// Grass is only ever legible close up: past thirty-odd metres a blade is well
/// under a pixel and all it can contribute is aliasing. Everything beyond this is
/// the flat `palette::GRASS` fill it has always been, which is why the ramp below
/// has to end *inside* the pool rather than at its edge.
const GRASS_RADIUS_M: f32 = 26.0;

/// Where blades start retiring, as a fraction of the radius. The shader staggers
/// each blade's own cut-off across this band, so the lawn thins towards its edge
/// instead of ending at a visible circle.
const FADE_START_FRAC: f32 = 0.55;

/// Altitude at which the whole layer starts fading, and where it is gone.
///
/// Far lower than the trees' band ([`TREE_FADE_START`] is 500 m) because the
/// thing being hidden is far smaller. It is a ramp rather than a switch for the
/// same reason every other altitude transition in this tree is: a step in the
/// middle of a climb is more noticeable than the detail it removes.
///
/// [`TREE_FADE_START`]: crate::systems::scatter::TREE_FADE_START
const GRASS_FADE_ALT: f32 = 60.0;
const GRASS_HIDE_ALT: f32 = 140.0;

// ─── The mask ────────────────────────────────────────────────────────────────

/// Side length of the mask texture, in texels, and the ground it covers.
///
/// 0.25 m per texel. That is finer than the disagreement between a landuse
/// polygon and the road ribbon drawn over it, which is the accuracy that actually
/// matters here — a coverage edge is only ever as true as the two generalisations
/// that produced it.
const FIELD_PX: u32 = 384;
const FIELD_M: f32 = 96.0;

/// How far inside the mask's edge the camera must stay. The rebuild fires when it
/// gets closer than this, so there is always at least a patch pool's worth of
/// valid mask ahead of the camera while the worker runs.
const FIELD_MARGIN_M: f32 = 8.0;

// ─── Material ────────────────────────────────────────────────────────────────

/// The one material every grass patch draws with.
pub type GrassMaterial = ExtendedMaterial<StandardMaterial, GrassExtension>;

/// Uniforms for `shaders/grass.wgsl`. Every one is read in the **vertex** stage —
/// this extension has no fragment shader at all, which is the point: the blade is
/// built here and then handed to `StandardMaterial` to be lit like any other
/// surface in the scene.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
pub struct GrassExtension {
    /// xy: the mask's minimum corner in anchor metres. z: its ground size.
    /// w: the ground plane the blades stand on.
    #[uniform(100, visibility(vertex))]
    pub field: Vec4,
    /// x: blade height. y: blade width. z: slot spacing (= jitter range).
    /// w: patch edge length, the unit `MeshTag` counts in.
    #[uniform(101, visibility(vertex))]
    pub blade: Vec4,
    /// xy: unit wind direction. z: sway amplitude. w: sway speed.
    #[uniform(102, visibility(vertex))]
    pub wind: Vec4,
    /// x: fade start (m). y: fade end (m). z: mask height origin. w: height span.
    #[uniform(103, visibility(vertex))]
    pub range: Vec4,
    /// rgb: tip colour, linear. a: how much darker the root is.
    #[uniform(104, visibility(vertex))]
    pub tint: Vec4,
    /// x: clump size (m). y, z: the density the thinnest and thickest patches
    /// keep. w: how far a blade's facing may swing off its clump's, in radians.
    #[uniform(107, visibility(vertex))]
    pub clump: Vec4,
    /// r: coverage. g: ground height. Sampled in the vertex stage, hence the
    /// visibility — the derive defaults a texture to fragment-only, and a
    /// vertex-stage sample against that fails bind-group validation at draw time
    /// rather than at compile time.
    #[texture(105, visibility(vertex))]
    #[sampler(106, visibility(vertex))]
    pub mask: Handle<Image>,
}

impl MaterialExtension for GrassExtension {
    fn vertex_shader() -> ShaderRef {
        "shaders/grass.wgsl".into()
    }
}

// ─── Resources ───────────────────────────────────────────────────────────────

/// Every live cell's grass, keyed and dropped exactly as the surface cells are.
///
/// Written by `map_geometry` through `CellStores` — it is at Bevy's
/// 16-parameter ceiling, so this rides in the same `SystemParam` bundle the
/// street labels and the walkable places do, and for the same reason: all three
/// are keyed by cell, inserted at the cell's origin, and must be dropped on
/// exactly the same events.
#[derive(Resource)]
pub struct GrassAreas {
    cells: std::collections::HashMap<(i64, i64), (Vec2, f32, Arc<GrassArea>)>,
    /// Bumped on every [`Self::clear`]. A re-anchor, a terrain revision or a
    /// source switch all clear the cells, and all three also invalidate the mask
    /// and the patch placement — which the patch system notices by comparing this.
    generation: u32,
    /// Bumped whenever the *set* of cells changes.
    ///
    /// Separate from `generation` because it means something weaker — the frame
    /// is still valid, there is just more (or less) ground to plant on — and
    /// because leaving it out is a real bug that a still capture hides: cells
    /// stream in over a second or two, so a mask built on the first one that
    /// landed is built over ground the camera is not standing on, comes back
    /// empty, and is then never rebuilt because the camera has not moved. That is
    /// a bare lawn with a healthy-looking `grass_patches` count.
    revision: u32,
    /// Mirror of `DataLayers::grass_enabled`, so the per-cell worker can see the
    /// toggle without `update_map_surfaces` taking another system parameter. Same
    /// contract as `WaterSeaState::shading` and `VegetationLod::enabled`.
    pub enabled: bool,
}

impl Default for GrassAreas {
    fn default() -> Self {
        Self {
            cells: std::collections::HashMap::new(),
            generation: 0,
            revision: 0,
            // Matches `DataLayers::grass_enabled`'s own default, which is the
            // tier's `detail_layers`. `data_layers` corrects it on the first
            // frame either way; starting from the tier means a phone does not
            // build one ring of grass areas before that happens.
            enabled: crate::quality::budget().detail_layers,
        }
    }
}

impl GrassAreas {
    /// Install one cell's grass, at the same origin `map_geometry` spawns its
    /// meshes at. A cell with no grass in it is dropped rather than stored.
    pub fn insert_cell(&mut self, cell: (i64, i64), origin: Vec2, tile_m: f32, area: GrassArea) {
        if area.is_empty() {
            return;
        }
        tracing::debug!(
            cell = ?cell,
            origin_x = origin.x,
            origin_z = origin.y,
            tile_m,
            triangles = area.triangles(),
            "grass area installed"
        );
        self.cells.insert(cell, (origin, tile_m, Arc::new(area)));
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn remove_cell(&mut self, cell: &(i64, i64)) {
        if self.cells.remove(cell).is_some() {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Drop every cell. Bumps the generation, which is what retires the mask and
    /// re-places the patches.
    /// The generation moves even when there was nothing to drop: a caller
    /// clearing an already-empty store is still saying the frame moved, and a
    /// re-anchor over bare ground would otherwise leave a stale mask standing.
    pub fn clear(&mut self) {
        self.cells.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    fn snapshot(&self) -> Vec<((i64, i64), Vec2, f32, Arc<GrassArea>)> {
        self.cells
            .iter()
            .map(|(c, (o, t, a))| (*c, *o, *t, a.clone()))
            .collect()
    }
}

/// The patch pool, the mask, and the state both are placed against.
#[derive(Resource)]
pub struct GrassPatches {
    /// The one lattice mesh, and the one material.
    mesh: Option<Handle<Mesh>>,
    material: Option<Handle<GrassMaterial>>,
    /// The pool, in the order it was spawned. Never grows or shrinks after the
    /// first placement.
    patches: Vec<Entity>,
    /// Patch-lattice offsets the pool covers, paired with `patches` by index.
    offsets: Vec<IVec2>,
    /// The lattice cell the pool is currently centred on, and the generation it
    /// was placed against.
    centre: Option<IVec2>,
    generation: u32,
    /// The mask currently bound to the material, and the rebuild in flight.
    field: Option<FieldPlacement>,
    /// `GrassAreas::revision` the bound mask was built against.
    field_revision: u32,
    pending: Arc<Mutex<Option<MaskField>>>,
    in_flight: bool,
    /// Wind and layer state already written to the material.
    applied_wind: Option<f32>,
    applied_alt: f32,
    /// Blades the pool would draw if every slot were on grass — the denominator
    /// behind `--dump-state`'s `grass_slots`.
    slots: u32,
    /// Patches the view drew last frame.
    ///
    /// Counted here rather than in the shot harness because that system is at
    /// Bevy's 16-parameter ceiling and a `Query` cannot join its `readouts` tuple
    /// (a tuple member has no lifetimes to name). One frame stale — bevy computes
    /// `ViewVisibility` in `PostUpdate`, after this — which is exactly right for a
    /// settled capture and meaningless mid-flight.
    rendered: usize,
}

/// Where the bound mask sits, in anchor metres.
struct FieldPlacement {
    origin: Vec2,
    size_m: f32,
}

impl Default for GrassPatches {
    fn default() -> Self {
        Self {
            mesh: None,
            material: None,
            patches: Vec::new(),
            offsets: Vec::new(),
            centre: None,
            generation: 0,
            field: None,
            field_revision: 0,
            pending: Arc::new(Mutex::new(None)),
            in_flight: false,
            applied_wind: None,
            applied_alt: f32::MAX,
            slots: 0,
            rendered: 0,
        }
    }
}

impl GrassPatches {
    /// Patch entities standing, the blade slots they add up to, and how many the
    /// view actually drew.
    ///
    /// Three numbers because there are three distinct failures and one metric
    /// cannot tell them apart. `0 / 0 / 0` is a pool that never spawned (the
    /// layer is off, or no cell holds grass). `n / m / 0` is a pool that is
    /// entirely outside the frustum — which is the *normal* state from any
    /// altitude, since the pool is laid out around the camera's ground position.
    /// `n / m / k` with a bare-looking lawn is the coverage mask: misplaced,
    /// unbuilt, or still on a worker.
    #[must_use]
    pub fn census(&self) -> (usize, u32, usize) {
        (self.patches.len(), self.slots, self.rendered)
    }

    /// Is a mask rebuild in flight? The `--shot` settle test waits on this: a
    /// capture taken while it is true photographs a lawn that has not arrived.
    #[must_use]
    pub fn streaming_busy(&self) -> bool {
        self.in_flight
    }
}

// ─── Systems ─────────────────────────────────────────────────────────────────

/// Place the pool, keep the mask under it, and drive the wind.
///
/// One system rather than three because all three are the same decision made
/// against the same camera position, and splitting them would mean three copies
/// of the "where is the camera, in which frame, and is this layer on at all"
/// preamble — plus an ordering between them that is not obvious from either end.
#[allow(clippy::too_many_arguments)]
pub fn update_grass(
    mut commands: Commands,
    mut patches: ResMut<GrassPatches>,
    mut areas: ResMut<GrassAreas>,
    dl: Res<crate::systems::data_layers::DataLayers>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<GrassMaterial>>,
    mut images: ResMut<Assets<Image>>,
    terrain: Res<TerrainField>,
    weather: Res<WeatherState>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    // `Without<FreeCamera>` is not redundant with `With<GrassPatch>`: Bevy proves
    // two queries disjoint from a `Without`, never from two different `With`s, so
    // this and the camera query above conflict on `Transform`/`GridCell` at
    // schedule build time and take the app down with a B0001 panic.
    mut placed: Query<
        (&mut Transform, &mut MeshTag, &mut GridCell),
        (With<GrassPatch>, Without<FreeCamera>),
    >,
    drawn: Query<&ViewVisibility, With<GrassPatch>>,
) {
    // Before any early return: a pool that has just been switched off must report
    // zero rather than keep last frame's count.
    patches.rendered = drawn.iter().filter(|v| v.get()).count();

    // Mirror the panel toggle where the per-cell worker can see it — the same
    // contract `VegetationLod::enabled` and `WaterSeaState::shading` have, and
    // for the same reason: `update_map_surfaces` is at Bevy's parameter ceiling
    // and cannot read `DataLayers` itself.
    areas.enabled = dl.grass_enabled;

    let Ok((cam_cell, cam_tf)) = camera.single() else {
        return;
    };
    let Ok(root) = bigspace.single() else {
        return;
    };

    // The camera in the anchored world-XZ frame — the same frame `map_geometry`
    // places its cell origins in, and the one the mask and the `MeshTag` lattice
    // are both measured in.
    let cam = Vec2::new(
        cam_cell.x as f32 * WORLD_CELL_EDGE + cam_tf.translation.x,
        cam_cell.z as f32 * WORLD_CELL_EDGE + cam_tf.translation.z,
    );
    let alt = globe.altitude_m(cam_cell, cam_tf) as f32;

    // Drain a finished mask whatever else happens, so a rebuild that lands after
    // the layer was switched off does not keep a worker slot marked busy forever.
    let landed = patches.pending.lock().ok().and_then(|mut g| g.take());
    if landed.is_some() {
        patches.in_flight = false;
    }

    let live = areas.enabled && alt < GRASS_HIDE_ALT && !areas.cells.is_empty();
    if !live {
        if !patches.patches.is_empty() {
            for e in patches.patches.drain(..) {
                commands.entity(e).despawn();
            }
            patches.offsets.clear();
            patches.centre = None;
            patches.slots = 0;
        }
        return;
    }

    // A re-anchor (or a terrain revision, or a source switch) moved every frame
    // this layer is placed in. Drop the mask and forget where the pool was, so
    // both are re-derived below rather than being nudged from stale state.
    if patches.generation != areas.generation {
        patches.generation = areas.generation;
        patches.field = None;
        patches.centre = None;
    }

    let (mesh, material) = ensure_assets(&mut patches, &mut meshes, &mut materials, &mut images);

    // ── The mask ──
    if let Some(field) = landed {
        let image = images.add(mask_image(&field));
        if let Some(mut m) = materials.get_mut(&material) {
            m.extension.field.x = field.origin[0];
            m.extension.field.y = field.origin[1];
            m.extension.field.z = field.size_m;
            m.extension.range.z = field.min_y;
            m.extension.range.w = field.span_y;
            m.extension.mask = image;
        }
        // The one line that separates the layer's three failures from each other:
        // an empty `cover` is a mask built over the wrong ground (or over none),
        // while a healthy `cover` with a bare lawn is the shader.
        let covered = field.data.chunks_exact(2).filter(|t| t[0] > 0).count();
        tracing::debug!(
            origin_x = field.origin[0],
            origin_z = field.origin[1],
            size_m = field.size_m,
            cover = format!("{:.3}", covered as f32 / (field.px * field.px) as f32),
            min_y = field.min_y,
            // The count the *job* saw, not the count now — those differ while
            // cells are still landing, and reporting the live one made an early
            // empty mask look as though it had been built over nine cells.
            cells = field.cells,
            "grass mask built"
        );
        patches.field = Some(FieldPlacement {
            origin: Vec2::new(field.origin[0], field.origin[1]),
            size_m: field.size_m,
        });
    }
    let need_field = patches.field_revision != areas.revision
        || patches.field.as_ref().is_none_or(|f| {
            let inner = f.size_m * 0.5 - GRASS_RADIUS_M - FIELD_MARGIN_M;
            (cam - (f.origin + Vec2::splat(f.size_m * 0.5)))
                .abs()
                .max_element()
                > inner.max(0.0)
        });
    if need_field && !patches.in_flight {
        spawn_mask_job(&patches, &areas, &terrain, cam);
        patches.in_flight = true;
        // Claimed here rather than when the job lands, so a cell arriving while
        // the worker runs queues exactly one more rebuild instead of a rebuild
        // per frame until it finishes.
        patches.field_revision = areas.revision;
    }

    // ── The wind, and the altitude ramp ──
    let wind_ms = weather.latest.map_or(UNKNOWN_WIND, |o| o.wind_ms).max(0.0);
    let fade = layer_fade(alt);
    let wind_moved = patches
        .applied_wind
        .is_none_or(|w| (w - wind_ms).abs() >= WIND_EPSILON);
    if wind_moved || (patches.applied_alt - fade).abs() > 0.01 {
        patches.applied_wind = Some(wind_ms);
        patches.applied_alt = fade;
        if let Some(mut m) = materials.get_mut(&material) {
            let dir = CLOUD_WIND.normalize_or(Vec2::X);
            let sway = CALM_SWAY.lerp(FULL_SWAY, (wind_ms / FULL_SWAY_WIND).clamp(0.0, 1.0));
            // The last term is the sway *rate*. It was 0.9 + 0.18·wind, which at
            // the light breeze this falls back to with no network is one lean
            // every four seconds — slow enough to read as a drifting mesh rather
            // than as wind.
            m.extension.wind = Vec4::new(dir.x, dir.y, sway, 1.6 + wind_ms * 0.30);
            // The altitude ramp is a *height* ramp, not an opacity one: there is
            // no alpha anywhere in this layer, and there must not be — a lawn is
            // tens of thousands of triangles and any of them being transparent
            // buys a sort order and a pass to fade something already sub-pixel.
            // Blades sink into the ground instead, which is also exactly what the
            // distance ramp does, so the two compose without a seam.
            m.extension.blade.x = BLADE_HEIGHT_M * fade;
        }
    }

    // ── The pool ──
    let centre = (cam / PATCH_M).floor().as_ivec2();
    if patches.offsets.is_empty() {
        patches.offsets = pool_offsets();
        patches.slots = patches.offsets.len() as u32 * slots() * slots();
    }
    // Spawn the pool once; thereafter a lattice crossing only *moves* it. That
    // is the whole design — see the module doc.
    if patches.patches.len() != patches.offsets.len() {
        for e in patches.patches.drain(..) {
            commands.entity(e).despawn();
        }
        let half = PATCH_M * 0.5;
        for _ in 0..patches.offsets.len() {
            let id = commands
                .spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    MeshTag(0),
                    Transform::default(),
                    GridCell::default(),
                    ChildOf(root),
                    GrassPatch,
                    HideInSpace,
                    // Thousands of two-centimetre casters is the shadow-map cost
                    // this scene can least afford (`quality::Budget::shadows`),
                    // for a contribution that is under a texel. Grass still
                    // *receives* shadows, which is the half that reads.
                    NotShadowCaster,
                    // The stored positions are the blade *slots*, all at y = 0, so
                    // a computed bounding box would be a flat sheet and Bevy would
                    // cull a patch the moment its ground plane left the frustum.
                    // The blades stand up out of it, and this is where that is
                    // declared.
                    Aabb::from_min_max(
                        Vec3::new(-half, -1.0, -half),
                        Vec3::new(half, BLADE_HEIGHT_M * 2.0, half),
                    ),
                ))
                .id();
            patches.patches.push(id);
        }
        // `centre` stays unset, so the placement below runs next frame — the
        // entities do not exist until these commands are applied, and a pool
        // placed on the frame it was queued would sit at the world origin
        // forever (the lattice test below would then think it was up to date).
        patches.centre = None;
        return;
    }

    if patches.centre == Some(centre) {
        return;
    }
    patches.centre = Some(centre);

    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    for (entity, offset) in patches.patches.iter().zip(patches.offsets.iter()) {
        let Ok((mut tf, mut tag, mut gc)) = placed.get_mut(*entity) else {
            continue;
        };
        let patch = centre + *offset;
        let world = Vec3::new(
            patch.x as f32 * PATCH_M,
            SurfaceGroup::Canopy.y_offset(),
            patch.y as f32 * PATCH_M,
        );
        let (cell, local) = grid.translation_to_grid(world.as_dvec3());
        tf.translation = local;
        *gc = cell;
        // Two signed 16-bit patch indices. This is the blade's anchor-metre frame
        // and the only thing that carries it into the shader — see the module doc
        // in `grass.wgsl` for why it cannot come from `world_position`.
        *tag = MeshTag(((patch.x as i16 as u16 as u32) << 16) | patch.y as i16 as u16 as u32);
    }
}

/// Marker for a pooled patch, so the pool can be found and retired.
#[derive(Component)]
pub struct GrassPatch;

/// How tall the blades stand at this altitude, 1 → 0 over the fade band.
fn layer_fade(alt: f32) -> f32 {
    let t = ((alt - GRASS_FADE_ALT) / (GRASS_HIDE_ALT - GRASS_FADE_ALT)).clamp(0.0, 1.0);
    // Smoothstep, for the same reason `vegetation::update_vegetation_lod` uses
    // one: the ends of a band are where a linear ramp shows its corners.
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// How far a patch's nearest point is from the pool centre, in metres.
///
/// A patch is *centred* on its lattice point (the lattice mesh runs ±half a patch
/// about its origin), so patch `i` reaches to within `(|i| − ½)·PATCH_M`. Using
/// the nearest point rather than the centre is what keeps the pool from having
/// bites taken out of its edge, and getting the half-patch right is what stops it
/// carrying a ring of patches whose every blade the distance ramp has already
/// retired.
fn patch_reach(offset: IVec2) -> f32 {
    let axis = |n: i32| (n.abs() as f32 - 0.5).max(0.0) * PATCH_M;
    Vec2::new(axis(offset.x), axis(offset.y)).length()
}

/// The patch-lattice offsets the pool covers. Computed once — it is a constant of
/// the tier.
fn pool_offsets() -> Vec<IVec2> {
    // A handset that turns this on by hand gets half the reach. The tier flag is
    // what it starts *off* under, so this is the "you asked for it anyway" case,
    // and a quarter of the vertex work is the honest thing to hand it.
    let radius = if crate::quality::budget().detail_layers {
        GRASS_RADIUS_M
    } else {
        GRASS_RADIUS_M * 0.5
    };
    let reach = (radius / PATCH_M + 0.5).ceil() as i32;
    let mut out = Vec::new();
    for j in -reach..=reach {
        for i in -reach..=reach {
            let offset = IVec2::new(i, j);
            if patch_reach(offset) <= radius {
                out.push(offset);
            }
        }
    }
    out
}

/// Create the shared mesh and material on first use.
fn ensure_assets(
    patches: &mut GrassPatches,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<GrassMaterial>,
    images: &mut Assets<Image>,
) -> (Handle<Mesh>, Handle<GrassMaterial>) {
    let mesh = patches
        .mesh
        .get_or_insert_with(|| meshes.add(lattice_mesh()))
        .clone();
    let material = patches
        .material
        .get_or_insert_with(|| {
            let empty = images.add(mask_image(&MaskField {
                px: 1,
                data: vec![0, 0],
                origin: [0.0, 0.0],
                size_m: 1.0,
                min_y: 0.0,
                span_y: 1.0,
                cells: 0,
            }));
            materials.add(GrassMaterial {
                base: StandardMaterial {
                    // The blade's own colour is written per vertex by the shader;
                    // this is the white it is multiplied into.
                    base_color: Color::WHITE,
                    perceptual_roughness: 0.9,
                    // The same rung the trees and the street furniture stand on,
                    // and for the same reason (`SurfaceGroup::depth_rung`): the
                    // coplanar ground sheets are biased *forward* to settle their
                    // own z-fight, so a blade at the default rung 0 is painted
                    // over by the very lawn it is growing out of. Real geometry
                    // standing on the ground belongs above the whole stack.
                    depth_bias: SurfaceGroup::Canopy.depth_bias(),
                    // A blade is a flat strip with no back to it: seen from the
                    // far side it is the same blade, and culling it would make a
                    // lawn half-vanish as the camera swung round.
                    double_sided: true,
                    cull_mode: None,
                    ..default()
                },
                extension: GrassExtension {
                    // Placed at nothing, so the first frame draws no blades
                    // rather than a lawn in the wrong place — the mask job below
                    // corrects it within a frame or two.
                    field: Vec4::new(0.0, 0.0, 1.0, SurfaceGroup::Canopy.y_offset()),
                    blade: Vec4::new(
                        BLADE_HEIGHT_M,
                        BLADE_WIDTH_M,
                        PATCH_M / slots() as f32 * JITTER_FRAC,
                        PATCH_M,
                    ),
                    wind: Vec4::new(1.0, 0.0, CALM_SWAY, 1.0),
                    range: Vec4::new(GRASS_RADIUS_M * FADE_START_FRAC, GRASS_RADIUS_M, 0.0, 1.0),
                    tint: blade_tint(),
                    clump: Vec4::new(CLUMP_M, DENSITY_LOW, DENSITY_HIGH, CLUMP_FAN),
                    mask: empty,
                },
            })
        })
        .clone();
    (mesh, material)
}

/// The blade's tip colour, and how much darker its root is.
///
/// Taken off `palette::GRASS` rather than picked: the flat fill is what the lawn
/// becomes at the edge of the pool and above the fade band, and a blade that is
/// not a shade of its own ground paints a coloured disc around the camera. It is
/// pulled a little deeper and greener than the paint because a blade catches the
/// sun on its face while the sheet under it does not — matched by eye against a
/// capture, which is the only way to match a lit surface to an unlit one.
fn blade_tint() -> Vec4 {
    let [r, g, b] = crate::systems::scatter::srgb_to_linear_rgb(palette::GRASS);
    // Only a little off the paint. An earlier 0.72 on blue read as straw against
    // the sage sheet it stands on — and the *ratio* between the two is a constant
    // here, so that much is judgeable without trusting the render. How the pair
    // actually lands once lit is not: colour and exposure are not evidence on the
    // container's software rasteriser, so the final value belongs on a
    // workstation (`docs/notes/headless-shots-software-renderer.md`).
    Vec4::new(r * 0.94, g * 0.99, b * 0.86, 0.45)
}

/// Build the shared lattice as a Bevy mesh.
fn lattice_mesh() -> Mesh {
    use bevy::render::mesh::{Indices, PrimitiveTopology};

    let side = slots();
    let lattice = turf::blade_lattice(side, PATCH_M / side as f32);
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        // `RENDER_WORLD` only: nothing ever reads a blade slot back, and this
        // asset is otherwise duplicated in `Assets<Mesh>` for the life of the app.
        RenderAssetUsages::RENDER_WORLD,
    );
    // The lattice is placed with its centre on the patch origin, so a patch's
    // `Transform` is its middle and the `Aabb` above is symmetric.
    let half = PATCH_M * 0.5;
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        lattice
            .positions
            .iter()
            .map(|p| [p[0] - half, p[1], p[2] - half])
            .collect::<Vec<_>>(),
    );
    // Normals and colours are placeholders the vertex shader overwrites. They are
    // present because their *presence* is what defines `VERTEX_NORMALS` and
    // `VERTEX_COLORS`, and so what gives the shader somewhere to write.
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, lattice.normals.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, lattice.uvs.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, lattice.colors.clone());
    mesh.insert_indices(Indices::U32(lattice.indices.clone()));
    mesh
}

/// Upload a mask as a two-channel texture.
///
/// `Rg8Unorm`, not sRGB: neither channel is a colour — red is a coverage and
/// green is a distance up. Linearly filtered, which is what turns a 25 cm texel
/// grid into a soft edge the shader can `smoothstep` across, and clamped so the
/// border row does not wrap a far field in from the opposite side (the shader
/// zeroes everything outside the field anyway, but a sampler that wraps would
/// make that check load-bearing rather than belt-and-braces).
fn mask_image(field: &MaskField) -> Image {
    let mut image = Image::new(
        Extent3d {
            width: field.px,
            height: field.px,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        field.data.clone(),
        TextureFormat::Rg8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        label: Some("grass mask".into()),
        ..ImageSamplerDescriptor::linear()
    });
    image
}

/// Rasterise the next mask on a worker.
///
/// On `platform::workers` rather than inline for the reason every other
/// per-cell product is: this is ~150k point-in-polygon tests, which is a frame on
/// a phone. It is also why the trigger has [`FIELD_MARGIN_M`] of slack — the
/// answer is allowed to take a while.
fn spawn_mask_job(patches: &GrassPatches, areas: &GrassAreas, terrain: &TerrainField, cam: Vec2) {
    let cells = areas.snapshot();
    let ground = terrain.snapshot();
    let sink = patches.pending.clone();
    let origin = cam - Vec2::splat(FIELD_M * 0.5);
    crate::platform::workers::submit(Box::new(move || {
        let placed: Vec<PlacedArea<'_>> = cells
            .iter()
            .map(|(_, o, t, a)| PlacedArea {
                origin: [o.x, o.y],
                tile_m: *t,
                area: a.as_ref(),
            })
            .collect();
        let field = turf::mask_field(&placed, [origin.x, origin.y], FIELD_M, FIELD_PX, &|x, z| {
            ground.at(x, z)
        });
        if let Ok(mut g) = sink.lock() {
            *g = Some(field);
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_covers_the_radius_and_not_much_more() {
        let offsets = pool_offsets();
        assert!(!offsets.is_empty());
        // Every patch must be reachable — a pool with a patch outside the fade
        // end is drawing a mesh whose every blade has already retired.
        for o in &offsets {
            assert!(
                patch_reach(*o) <= GRASS_RADIUS_M + 1e-3,
                "{o:?} is out of range"
            );
        }
        // …and the disc must be covered: the patch under the camera and its four
        // neighbours are the ones a standing avatar is actually looking at.
        for o in [IVec2::ZERO, IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y] {
            assert!(offsets.contains(&o), "{o:?} missing from the pool");
        }
    }

    #[test]
    fn the_altitude_ramp_is_continuous_at_both_ends() {
        assert!((layer_fade(0.0) - 1.0).abs() < 1e-6);
        assert!((layer_fade(GRASS_FADE_ALT) - 1.0).abs() < 1e-6);
        assert!(layer_fade(GRASS_HIDE_ALT).abs() < 1e-6);
        assert!(layer_fade(GRASS_HIDE_ALT * 10.0).abs() < 1e-6);
        // Monotone: a climb must never make the grass taller.
        let mut prev = 1.0;
        for i in 0..=200 {
            let f = layer_fade(i as f32 * 2.0);
            assert!(f <= prev + 1e-6, "ramp rose at {i}");
            prev = f;
        }
    }

    #[test]
    fn a_patch_index_survives_the_tag_round_trip() {
        // The shader sign-extends two i16s out of the tag. If the pack and the
        // unpack ever disagree, every patch west or north of the anchor samples
        // the mask sixty kilometres away — which renders as bare ground, with
        // nothing anywhere to say why.
        for (x, y) in [(0, 0), (1, -1), (-1, 1), (-32768, 32767), (5000, -5000)] {
            let tag = ((x as i16 as u16 as u32) << 16) | (y as i16 as u16 as u32);
            let signed = tag as i32;
            assert_eq!(signed >> 16, x, "high half of {tag:#010x}");
            assert_eq!((signed << 16) >> 16, y, "low half of {tag:#010x}");
        }
    }

    #[test]
    fn the_lattice_mesh_carries_every_attribute_the_shader_declares() {
        // `grass.wgsl` declares locations 0, 1, 2 and 5 exactly. A missing
        // attribute does not fail to compile — it drops the matching shader-def,
        // and the pipeline is then built against a vertex struct the mesh does
        // not supply, which wgpu rejects at draw time.
        let mesh = lattice_mesh();
        for attr in [
            Mesh::ATTRIBUTE_POSITION,
            Mesh::ATTRIBUTE_NORMAL,
            Mesh::ATTRIBUTE_UV_0,
            Mesh::ATTRIBUTE_COLOR,
        ] {
            assert!(
                mesh.attribute(attr).is_some(),
                "the lattice is missing an attribute the shader reads"
            );
        }
        assert_eq!(
            mesh.count_vertices(),
            (slots() * slots() * turf::BLADE_VERTS) as usize
        );
    }

    #[test]
    fn the_lattice_is_centred_on_its_patch() {
        // The patch `Transform` is its middle and the `Aabb` is symmetric about
        // it; a lattice laid from a corner instead would put every patch half a
        // patch off its own lattice slot, which reads as grass sliding under the
        // camera.
        let mesh = lattice_mesh();
        let Some(bevy::render::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("positions are Float32x3");
        };
        let half = PATCH_M * 0.5;
        for v in p {
            assert!(v[0] >= -half && v[0] <= half, "x {} off-patch", v[0]);
            assert!(v[2] >= -half && v[2] <= half, "z {} off-patch", v[2]);
        }
    }
}
