//! Near-field grass — the ECS half: the patch pool, the moving mask, the wind and
//! the altitude ramp.
//!
//! The pure half is [`crate::systems::turf`] and the geometry is
//! `assets/shaders/grass.wesl`; both have the reasoning for *why* a blade is
//! generated in the vertex shader rather than scattered and uploaded. What this
//! module owns is the consequence of that decision, which is that grass does not
//! stream. Per [`Tier`] — a dense near pool, and on the full quality rung two
//! progressively sparser rings of wider blades that push the lawn's edge out to
//! where a blade is sub-pixel — there is:
//!
//! * **one mesh** — [`turf::blade_lattice`], uploaded at startup and never again;
//! * **one material** — rewritten when the wind changes (a few times an hour) and
//!   when the mask moves (every twenty-odd metres of walking);
//! * **a fixed pool of patch entities**, spawned once, which slide along a
//!   world-aligned lattice under the camera. A patch never spawns, never
//!   despawns, and never has its mesh rebuilt: crossing a lattice line moves a
//!   `Transform` and rewrites a `MeshTag`.
//!
//! The rings hand over statistically, not geometrically — see [`Tier`] — so
//! there is no LOD *switch* anywhere: no distance at which anything pops, and no
//! circle on the ground where the density changes.
//!
//! That last point is the whole reason this layer exists in the shape it does.
//! Every other streamer in this tree is bounded per frame because mass mesh churn
//! loses the device (`docs/notes/upload-bursts.md`), and grass at a
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

use crate::platform::sync::{Arc, Mutex};
use bevy::asset::RenderAssetUsages;
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::ViewVisibility;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use bevy::render::mesh::MeshTag;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use big_space::prelude::{Grid, GridCell};

use crate::components::{FreeCamera, HideInSpace};
use crate::grid::{anchored_xz, CameraAndRoot, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::globe::GlobeState;
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
/// patch, so this and [`PATCH_M`] together are the density: 152² over 64 m² is
/// ~361 blades/m².
///
/// **Density is the one number that decides whether this reads as a lawn**, and
/// it took four captures over the Noorderplantsoen on 2026-08-20 to find: 9
/// blades/m² read as scattered weeds, 16 as a sparse field, 25 as tufts with bare
/// ground between them. Grass only stops looking like individual plants when the
/// blades overlap, and no amount of getting the blade *shape* right substitutes
/// for that. Real turf is thousands per m²; this is as close as a real-time
/// budget gets (raised from 196/m² on 2026-08-21 because the ground still read
/// through the lawn, and again from 256/m² on 2026-09-10 on an explicit ask for
/// a lusher world — density, the survival floor and the blade width moved
/// together the first time, since coverage is their product).
///
/// It costs nothing to *store* — the lattice is one shared mesh — so what
/// density actually buys is paid in **vertex invocations**: ~116k per drawn near
/// patch, and a near pool with ~17 patches in frame is ~2.0M a frame, against
/// ~1.4M at the old 128. That is cheap on a desktop GPU and is not free on a
/// tiler, which is why the handset rung takes both a lower count here *and* half
/// the radius in [`pool_offsets`] — a quarter of the area at a quarter of the
/// density. **Android auto-detects to `Budget::balanced`, which sets
/// `detail_layers`** — so a phone runs this branch, not the 64 below, and this
/// number lands on the device least able to afford it. Battery saver
/// (`Budget::phone`) is the way out.
fn slots() -> u32 {
    if crate::quality::budget().detail_layers {
        152
    } else {
        64
    }
}

/// One ring of the lawn: a lattice density, a patch size, and the distance band
/// its blades live in.
///
/// The **arrive** band is what makes two rings read as one lawn. A far blade
/// rises out of the ground across the exact band the near blades sink into it,
/// and both draw their per-blade cut from the same hash lane (`h.z` in the
/// shader) — so at any distance through the handover the *expected* blade count
/// falls smoothly from the near density to the far one, with no circle anywhere
/// for the eye to find. The near tier's arrive band is placed below zero, which
/// the shader reads as "always arrived".
#[derive(Clone, Copy)]
struct Tier {
    patch_m: f32,
    slots: u32,
    /// Distance band over which blades stand up, staggered per blade.
    arrive: Vec2,
    /// Distance band over which blades retire, staggered per blade.
    retire: Vec2,
    height_m: f32,
    width_m: f32,
}

/// The rings the current quality tier runs, innermost first — and the order is
/// load-bearing, because the handover chain is expressed as "each ring arrives
/// exactly where the one inside it retires" and the test walks consecutive pairs.
///
/// The outer two rings are full-tier only: they exist to push the lawn's edge out
/// to where a blade is sub-pixel, and a handset that forces the layer on gets the
/// honest half-radius near pool it always did.
fn tiers() -> Vec<Tier> {
    let detail = crate::quality::budget().detail_layers;
    let near_radius = if detail {
        GRASS_RADIUS_M
    } else {
        // A handset that turns this on by hand gets half the reach. The tier
        // flag is what it starts *off* under, so this is the "you asked for it
        // anyway" case, and a quarter of the vertex work is the honest thing to
        // hand it.
        GRASS_RADIUS_M * 0.5
    };
    let mut out = vec![Tier {
        patch_m: PATCH_M,
        slots: slots(),
        arrive: Vec2::new(-4.0, -2.0),
        retire: Vec2::new(near_radius * FADE_START_FRAC, near_radius),
        height_m: BLADE_HEIGHT_M,
        width_m: BLADE_WIDTH_M,
    }];
    if detail {
        out.push(Tier {
            patch_m: FAR_PATCH_M,
            slots: FAR_SLOTS,
            // Exactly the near tier's retire band — see [`Tier`].
            arrive: Vec2::new(GRASS_RADIUS_M * FADE_START_FRAC, GRASS_RADIUS_M),
            retire: Vec2::new(FAR_RADIUS_M * FADE_START_FRAC, FAR_RADIUS_M),
            height_m: FAR_BLADE_HEIGHT_M,
            width_m: FAR_BLADE_WIDTH_M,
        });
        out.push(Tier {
            patch_m: HORIZON_PATCH_M,
            slots: HORIZON_SLOTS,
            // …and again: exactly the far tier's retire band, so the chain of
            // handovers is one rule applied twice rather than two tunings.
            arrive: Vec2::new(FAR_RADIUS_M * FADE_START_FRAC, FAR_RADIUS_M),
            retire: Vec2::new(HORIZON_RADIUS_M * FADE_START_FRAC, HORIZON_RADIUS_M),
            height_m: HORIZON_BLADE_HEIGHT_M,
            width_m: HORIZON_BLADE_WIDTH_M,
        });
    }
    out
}

/// The distance the last blade of any ring has retired by — what the mask has to
/// cover and the rebuild trigger has to leave slack for.
fn max_radius() -> f32 {
    tiers().iter().map(|t| t.retire.y).fold(0.0, f32::max)
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
/// exist to distinguish. Thin, not absent. It is also the *free* density knob —
/// a dead slot costs its vertex invocations regardless, so raising the floor
/// thickens the sparse patches at zero extra vertex work (raised 0.34 → 0.5 on
/// 2026-08-21, when the ground still read through the lawn).
const DENSITY_LOW: f32 = 0.5;
const DENSITY_HIGH: f32 = 1.0;

/// How far a blade's facing may swing off its clump's, in radians. Wide enough
/// that a tuft fans, narrow enough that it is recognisably one tuft.
const CLUMP_FAN: f32 = 2.0;

/// Blade height and width in metres, before the shader's per-blade jitter.
///
/// Wild grass, not a lawn — an explicit direction change (2026-08-21). The
/// layer began deliberately mown at 15 cm; the ask is an unmown verge, so the
/// height is shin-high and the *variation* carries the wildness: the shader
/// spreads per-blade height 0.6–1.4× and per-clump 0.65–1.35×, so a field is
/// patchy stands rather than a cut surface, and the droop is heavy enough that
/// tall blades fold over. The old warning stands in one respect: this is still
/// well short of the knee-high reeds the first 26 cm × 2.6 cm attempt produced,
/// because that was a *stiff* 26 cm — height reads as wild only together with
/// droop and unevenness, which is where the rest of this tune lives.
const BLADE_HEIGHT_M: f32 = 0.38;
/// Still narrower than a real blade looks from standing height, but wide enough
/// that at [`slots`]'s density the blades overlap and the ground stops reading
/// through the lawn — that was the explicit ask on 2026-08-21. The root
/// darkening is what keeps an overlapping mat from going flat: the depth now
/// comes from the tone gradient rather than from bare ground between blades.
const BLADE_WIDTH_M: f32 = 0.013;

/// How far a blade may be jittered off its lattice slot, as a multiple of the
/// slot spacing — the shader offsets by ±half of this.
///
/// **Deliberately past 1.0, so a blade can leave the cell it was born in.** At
/// the old 0.9 it could not: ±0.45 of a cell is textbook stratified sampling,
/// which is a *good* point set for integration and the wrong one for a lawn,
/// because every blade stays in its own row and the rows stay visible. Seen along
/// either axis at a grazing angle that reads as corduroy — the artefact reported
/// on 2026-09-11, and no amount of jitter under half a cell can remove it,
/// because the constraint *is* the artefact.
///
/// Past 1.0 two neighbours can occupy the same ground and a cell can come up
/// empty. Both are wanted: real grass grows in clumps with gaps between them, and
/// the density this trades away is statistical only — the blade *count* is still
/// exactly `slots²`, since every slot still draws.
///
/// The other half of the fix is that the lattice's rows are staggered half a cell
/// against each other (`turf::blade_lattice`). Neither alone is enough: stagger
/// without jitter is a neat hexagonal grid, and jitter without stagger is what
/// this was.
///
/// It is paid for in the patch's `Aabb`, which has to grow by the jitter radius
/// or the blades that wander out of their patch pop at the frustum edge.
const JITTER_FRAC: f32 = 1.3;

/// Sway amplitude at [`FULL_SWAY_WIND`], as a fraction of blade height, and the
/// wind speed the ramp saturates at.
///
/// The same shape `water::sea_state` uses and for the same reason: a still lawn
/// is a real state but a rare one, and one that reads as "the shader isn't
/// running", so the calm end is a stir rather than nothing.
const CALM_SWAY: f32 = 0.10;
const FULL_SWAY: f32 = 0.55;
const FULL_SWAY_WIND: f32 = 12.0;

/// Wind change (m/s) needed before the material is rewritten. The animation clock
/// is `globals.time` inside the shader, so this system tracks *weather*, which
/// moves on the scale of minutes.
const WIND_EPSILON: f32 = 0.25;

// ─── How far it reaches ──────────────────────────────────────────────────────

/// Radius of the *near* pool, in metres, at the full quality tier — and the
/// distance by which the last dense blade has retired.
///
/// A blade at full density is only legible close up; past this it hands over to
/// the far ring below, whose blades are sparser and wider. On the reduced tier
/// (a handset that turned the layer on by hand) there is no far ring and this
/// halved radius is where the flat `palette::GRASS` fill takes over.
const GRASS_RADIUS_M: f32 = 26.0;

/// Where blades start retiring, as a fraction of their tier's radius. The shader
/// staggers each blade's own cut-off across this band, so a tier thins towards
/// its edge instead of ending at a visible circle.
///
/// 0.78, up from 0.55 (2026-08-21): with the band starting at 55% the near pool
/// was already thinning from 14 m out, so most of what a standing viewer looks
/// at was the half-density tail — raising the *lattice* density changed almost
/// nothing on screen because the retire ramp had already given it back. Full
/// density now holds to ~20 m and the handover band is the last quarter.
const FADE_START_FRAC: f32 = 0.78;

/// Radius of the far ring — where its blades have retired and
/// [`HORIZON_RADIUS_M`]'s ring has fully arrived. It was the outermost ring until
/// 2026-09-10 and the flat fill took over here; the third ring now carries the
/// lawn on, and this is a handover between two sets of blades rather than
/// between blades and paint.
const FAR_RADIUS_M: f32 = 80.0;

/// Patch edge and lattice side of the far ring: 88² slots over a 16 m patch is
/// ~30 blades/m² against the near pool's ~256. One coarse blade stands in for
/// eight — Ghost of Tsushima's "drop three blades in four at the LOD switch"
/// made into a ring — and the bigger patch keeps a drawn far patch at half a
/// near patch's vertex cost while covering four times the ground.
const FAR_PATCH_M: f32 = 16.0;
const FAR_SLOTS: u32 = 88;

/// A far blade stands in for eight near ones, so it is taller and nearly four
/// times wider. Width times height times density is the ground coverage, and it
/// has to survive the grazing-angle projection: at 30 m a standing viewer sees
/// the ground foreshortened ~8:1 while a vertical blade is not, so ~0.3 m² of
/// blade silhouette per m² of ground is what closes the carpet in expectation.
/// The first far ring (20/m² × 4 cm × 17 cm ≈ 0.14) left a third of the ground
/// reading through at mid-field, which is exactly what it looked like. Safe
/// *because* the ring only exists past the distance where a single blade's
/// silhouette can be read.
const FAR_BLADE_HEIGHT_M: f32 = 0.42;
const FAR_BLADE_WIDTH_M: f32 = 0.050;

/// The third ring, and the same argument one rung coarser: it carries the lawn
/// from where the far ring retires out to [`HORIZON_RADIUS_M`], where a blade of
/// any width is well under a pixel and the flat fill can take over without the
/// eye finding the join.
///
/// Extending the *far* ring to 130 m instead was the obvious move and is the
/// expensive one: its 30 blades/m² over an annulus 2.6× the area is ~2.5M extra
/// vertex invocations a frame, all of them spent on blades nobody can resolve.
/// A ring is the cheap shape because density may fall with distance —
/// 96² slots on a 32 m patch is ~9 blades/m², a third of the far ring's and a
/// twenty-eighth of the near one's — and the handover is statistical, so a
/// sparser ring costs no seam.
///
/// The blade is wide and tall for the reason [`FAR_BLADE_WIDTH_M`] gives, taken
/// one step further: width × height × density is the ground coverage that has to
/// survive the grazing-angle projection, and at 9 blades/m² closing the carpet
/// needs ~0.5 m² of silhouette per m² (9 × 0.11 × 0.50 ≈ 0.50, against the far
/// ring's 0.63). Safe *because* it only exists past 80 m: a 11 cm blade would be
/// a plank at walking distance and is a texel at this one.
const HORIZON_RADIUS_M: f32 = 130.0;
const HORIZON_PATCH_M: f32 = 32.0;
const HORIZON_SLOTS: u32 = 96;
const HORIZON_BLADE_HEIGHT_M: f32 = 0.50;
const HORIZON_BLADE_WIDTH_M: f32 = 0.11;

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

/// Side length of the mask texture, in texels, and the ground it covers, per
/// quality tier.
///
/// The reduced tier is 0.25 m per texel — finer than the disagreement between a
/// landuse polygon and the road ribbon drawn over it, which is the accuracy that
/// actually matters here; a coverage edge is only ever as true as the two
/// generalisations that produced it. The full tier has to reach the outermost
/// ring, so it trades to 0.375 m per texel over a 300 m field: still under the
/// generalisation error, and the linear filter plus the probabilistic survival
/// test feather the edge either way. One mask serves all three rings — the outer
/// blades need no more coverage accuracy, just coverage at all, and a second
/// coarser mask would double the worker/rebuild machinery to save ~600 KB.
///
/// The field is sized by the test, not by eye: it must hold
/// `max_radius() + FIELD_MARGIN_M` with a patch's worth to spare, so extending a
/// ring without growing this silently clips the outermost blades to the mask's
/// edge — a circular haircut that looks like a coverage bug. Growing it is what
/// the reach costs off-screen: 800² texels against 640² is 1.6× the ~150k
/// point-in-polygon tests a rebuild does on a worker, every twenty-odd metres of
/// walking.
fn field_dims() -> (f32, u32) {
    if crate::quality::budget().detail_layers {
        (300.0, 800)
    } else {
        (96.0, 384)
    }
}

/// How far inside the mask's edge the camera must stay. The rebuild fires when it
/// gets closer than this, so there is always at least a patch pool's worth of
/// valid mask ahead of the camera while the worker runs.
const FIELD_MARGIN_M: f32 = 8.0;

// ─── Material ────────────────────────────────────────────────────────────────

// The grass material lives in `cartopolis_materials`, so that Bevy's
// per-material plugin code is generated there rather than in this crate.
pub use cartopolis_materials::grass::{GrassExtension, GrassMaterial};

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

/// One tier's pool: its lattice mesh, its material, and the entities that slide
/// along its own patch lattice. The mesh and material outlive a layer toggle;
/// only `patches` is drained when the layer goes quiet.
struct TierPool {
    tier: Tier,
    mesh: Handle<Mesh>,
    material: Handle<GrassMaterial>,
    /// The pool, in the order it was spawned. Never grows or shrinks after the
    /// first placement.
    patches: Vec<Entity>,
    /// Patch-lattice offsets the pool covers, paired with `patches` by index.
    offsets: Vec<IVec2>,
    /// The lattice cell the pool is currently centred on. In *this tier's*
    /// patch units — the far ring's lattice is coarser than the near pool's, so
    /// the two cross lines at different moments and each keeps its own centre.
    centre: Option<IVec2>,
}

/// The patch pools (one per [`Tier`]), the mask, and the state all of it is
/// placed against.
#[derive(Resource)]
pub struct GrassPatches {
    pools: Vec<TierPool>,
    generation: u32,
    /// The mask currently bound to the materials, and the rebuild in flight.
    field: Option<FieldPlacement>,
    /// `GrassAreas::revision` the bound mask was built against.
    field_revision: u32,
    pending: Arc<Mutex<Option<MaskField>>>,
    in_flight: bool,
    /// Wind (the surface velocity, m/s) and layer state already written to the
    /// materials.
    applied_wind: Option<Vec2>,
    applied_alt: f32,
    /// Patches the view drew last frame, across every tier.
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
            pools: Vec::new(),
            generation: 0,
            field: None,
            field_revision: 0,
            pending: Arc::new(Mutex::new(None)),
            in_flight: false,
            applied_wind: None,
            applied_alt: f32::MAX,
            rendered: 0,
        }
    }
}

impl crate::systems::stream_watch::StreamBusy for GrassPatches {
    const NAME: &'static str = "grass";
    fn stream_busy(&self) -> bool {
        self.streaming_busy()
    }
}

impl GrassPatches {
    /// Patch entities standing (all tiers), the blade slots they add up to, and
    /// how many the view actually drew.
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
        let patches: usize = self.pools.iter().map(|p| p.patches.len()).sum();
        let slots: u32 = self
            .pools
            .iter()
            .filter(|p| !p.patches.is_empty())
            .map(|p| p.offsets.len() as u32 * p.tier.slots * p.tier.slots)
            .sum();
        (patches, slots, self.rendered)
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
    tunnels: Res<crate::systems::tunnels::Tunnels>,
    weather: Res<WeatherState>,
    globe: Res<GlobeState>,
    // The camera and the `BigSpace` root as one parameter: this system is one
    // short of Bevy's sixteen.
    view: CameraAndRoot,
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

    let Some((cam_cell, cam_tf)) = view.camera.get() else {
        return;
    };
    let Ok(root) = view.root.single() else {
        return;
    };

    // The camera in the anchored world-XZ frame — the same frame `map_geometry`
    // places its cell origins in, and the one the mask and the `MeshTag` lattice
    // are both measured in.
    let cam = anchored_xz(cam_cell, cam_tf.translation);
    let alt = globe.altitude_m(cam_cell, cam_tf) as f32;

    // Drain a finished mask whatever else happens, so a rebuild that lands after
    // the layer was switched off does not keep a worker slot marked busy forever.
    let landed = patches.pending.lock().ok().and_then(|mut g| g.take());
    if landed.is_some() {
        patches.in_flight = false;
    }

    let live = areas.enabled && alt < GRASS_HIDE_ALT && !areas.cells.is_empty();
    if !live {
        for pool in &mut patches.pools {
            for e in pool.patches.drain(..) {
                commands.entity(e).despawn();
            }
            pool.centre = None;
        }
        return;
    }

    // A re-anchor (or a terrain revision, or a source switch) moved every frame
    // this layer is placed in. Drop the mask and forget where the pool was, so
    // both are re-derived below rather than being nudged from stale state.
    if patches.generation != areas.generation {
        patches.generation = areas.generation;
        patches.field = None;
        for pool in &mut patches.pools {
            pool.centre = None;
        }
    }

    ensure_pools(&mut patches, &mut meshes, &mut materials, &mut images);

    // ── The mask ──
    if let Some(mut field) = landed {
        // Counted *before* the buffer moves into the image below — the debug line
        // further down needs it, and the mask is 384²×2 bytes, so handing it over
        // rather than copying it is the difference between a move and an 800 kB
        // memcpy every time a new field lands.
        let covered = field.data.chunks_exact(2).filter(|t| t[0] > 0).count();
        let image = images.add(mask_image(field.px, std::mem::take(&mut field.data)));
        for pool in &patches.pools {
            if let Some(mut m) = materials.get_mut(&pool.material) {
                m.extension.field.x = field.origin[0];
                m.extension.field.y = field.origin[1];
                m.extension.field.z = field.size_m;
                m.extension.range.z = field.min_y;
                m.extension.range.w = field.span_y;
                m.extension.mask = image.clone();
            }
        }
        // The one line that separates the layer's three failures from each other:
        // an empty `cover` is a mask built over the wrong ground (or over none),
        // while a healthy `cover` with a bare lawn is the shader. (`covered` is
        // computed above, before `data` is moved into the image.)
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
            let inner = f.size_m * 0.5 - max_radius() - FIELD_MARGIN_M;
            (cam - (f.origin + Vec2::splat(f.size_m * 0.5)))
                .abs()
                .max_element()
                > inner.max(0.0)
        });
    if need_field && !patches.in_flight {
        spawn_mask_job(&patches, &areas, &terrain, &tunnels, cam);
        patches.in_flight = true;
        // Claimed here rather than when the job lands, so a cell arriving while
        // the worker runs queues exactly one more rebuild instead of a rebuild
        // per frame until it finishes.
        patches.field_revision = areas.revision;
    }

    // ── The wind, and the altitude ramp ──
    // The live surface wind, with the same light south-westerly the water and the
    // clouds fall back to until a weather sample lands — they are the same wind.
    let wind = crate::systems::weather::surface_wind(&weather);
    let wind_ms = wind.length();
    let fade = layer_fade(alt);
    let wind_moved = patches
        .applied_wind
        .is_none_or(|w| (w - wind).length() >= WIND_EPSILON);
    if wind_moved || (patches.applied_alt - fade).abs() > 0.01 {
        patches.applied_wind = Some(wind);
        patches.applied_alt = fade;
        let dir = wind.normalize_or(Vec2::X);
        let sway = CALM_SWAY.lerp(FULL_SWAY, (wind_ms / FULL_SWAY_WIND).clamp(0.0, 1.0));
        for pool in &patches.pools {
            if let Some(mut m) = materials.get_mut(&pool.material) {
                // The last term is the sway *rate*. It was 0.9 + 0.18·wind, which
                // at the light breeze this falls back to with no network is one
                // lean every four seconds — slow enough to read as a drifting
                // mesh rather than as wind.
                m.extension.wind = Vec4::new(dir.x, dir.y, sway, 1.6 + wind_ms * 0.30);
                // The altitude ramp is a *height* ramp, not an opacity one: there
                // is no alpha anywhere in this layer, and there must not be — a
                // lawn is tens of thousands of triangles and any of them being
                // transparent buys a sort order and a pass to fade something
                // already sub-pixel. Blades sink into the ground instead, which
                // is also exactly what the distance ramp does, so the two compose
                // without a seam.
                m.extension.blade.x = pool.tier.height_m * fade;
            }
        }
    }

    // ── The pools ──
    // Spawn each pool once; thereafter a lattice crossing only *moves* it. That
    // is the whole design — see the module doc.
    let mut spawned = false;
    for pool in &mut patches.pools {
        if pool.patches.len() == pool.offsets.len() {
            continue;
        }
        for e in pool.patches.drain(..) {
            commands.entity(e).despawn();
        }
        let half = pool.tier.patch_m * 0.5;
        // How far the shader may throw a blade off its slot, in metres — the same
        // product `update_grass` hands the material, and the reason the box below
        // is wider than the patch.
        let jitter_m = pool.tier.patch_m / pool.tier.slots as f32 * JITTER_FRAC * 0.5;
        for _ in 0..pool.offsets.len() {
            let id = commands
                .spawn((
                    Mesh3d(pool.mesh.clone()),
                    MeshMaterial3d(pool.material.clone()),
                    MeshTag::new(0),
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
                    // declared. ×2.5 covers the shader's stacked height jitters
                    // (per-blade 1.4 × per-clump 1.35 × vigour 1.08 ≈ 2.05) with
                    // slack — a blade taller than its patch's Aabb pops out at
                    // the frustum edge with nothing anywhere to say why.
                    //
                    // The *lateral* slack is the same rule in the other axis, and
                    // it is new with `JITTER_FRAC` past 1.0: a blade may now be
                    // offset more than half a cell, so the outermost row leaves
                    // the patch it belongs to. Without this the box still claims
                    // the old bounds and those blades wink out at the screen edge.
                    Aabb::from_min_max(
                        Vec3::new(-half - jitter_m, -1.0, -half - jitter_m),
                        Vec3::new(half + jitter_m, pool.tier.height_m * 2.5, half + jitter_m),
                    ),
                ))
                .id();
            pool.patches.push(id);
        }
        // `centre` stays unset, so the placement below runs next frame — the
        // entities do not exist until these commands are applied, and a pool
        // placed on the frame it was queued would sit at the world origin
        // forever (the lattice test below would then think it was up to date).
        pool.centre = None;
        spawned = true;
    }
    if spawned {
        return;
    }

    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    for pool in &mut patches.pools {
        let centre = (cam / pool.tier.patch_m).floor().as_ivec2();
        if pool.centre == Some(centre) {
            continue;
        }
        pool.centre = Some(centre);

        for (entity, offset) in pool.patches.iter().zip(pool.offsets.iter()) {
            let Ok((mut tf, mut tag, mut gc)) = placed.get_mut(*entity) else {
                continue;
            };
            let patch = centre + *offset;
            let world = Vec3::new(
                patch.x as f32 * pool.tier.patch_m,
                SurfaceGroup::Canopy.y_offset(),
                patch.y as f32 * pool.tier.patch_m,
            );
            let (cell, local) = grid.translation_to_grid(world.as_dvec3());
            tf.translation = local;
            *gc = cell;
            // Two signed 16-bit patch indices. This is the blade's anchor-metre
            // frame and the only thing that carries it into the shader — see the
            // module doc in `grass.wesl` for why it cannot come from
            // `world_position`. The unit is this tier's own patch edge, which
            // the shader reads back out of `blade.w`.
            *tag =
                MeshTag::new(((patch.x as i16 as u16 as u32) << 16) | patch.y as i16 as u16 as u32);
        }
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
/// about its origin), so patch `i` reaches to within `(|i| − ½)·patch_m`. Using
/// the nearest point rather than the centre is what keeps the pool from having
/// bites taken out of its edge, and getting the half-patch right is what stops it
/// carrying a ring of patches whose every blade the distance ramp has already
/// retired.
fn patch_reach(offset: IVec2, patch_m: f32) -> f32 {
    let axis = |n: i32| (n.abs() as f32 - 0.5).max(0.0) * patch_m;
    Vec2::new(axis(offset.x), axis(offset.y)).length()
}

/// How far a patch's *farthest* point is from the pool centre — the other half of
/// the annulus test: a far-ring patch whose whole extent sits nearer than the
/// arrive band would draw a mesh no blade of which ever stands.
fn patch_far_reach(offset: IVec2, patch_m: f32) -> f32 {
    let axis = |n: i32| (n.abs() as f32 + 0.5) * patch_m;
    Vec2::new(axis(offset.x), axis(offset.y)).length()
}

/// The patch-lattice offsets a tier's pool covers. Computed once — it is a
/// constant of the tier. A disc for the near pool; an annulus for the far ring,
/// whose hole is the ground the near pool still owns.
fn pool_offsets(tier: &Tier) -> Vec<IVec2> {
    let radius = tier.retire.y;
    let reach = (radius / tier.patch_m + 0.5).ceil() as i32;
    let mut out = Vec::new();
    for j in -reach..=reach {
        for i in -reach..=reach {
            let offset = IVec2::new(i, j);
            // The −2 is the smoothstep lead-in the shader gives an arriving
            // blade, so a patch straddling the very start of the band is kept.
            if patch_reach(offset, tier.patch_m) <= radius
                && patch_far_reach(offset, tier.patch_m) >= tier.arrive.x - 2.0
            {
                out.push(offset);
            }
        }
    }
    out
}

/// Create each tier's mesh, material and offsets on first use.
fn ensure_pools(
    patches: &mut GrassPatches,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<GrassMaterial>,
    images: &mut Assets<Image>,
) {
    if !patches.pools.is_empty() {
        return;
    }
    // One texel of "no grass here", standing in until a real field lands.
    let empty = images.add(mask_image(1, vec![0, 0]));
    for tier in tiers() {
        let material = materials.add(GrassMaterial {
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
                // rather than a lawn in the wrong place — the mask job
                // corrects it within a frame or two.
                field: Vec4::new(0.0, 0.0, 1.0, SurfaceGroup::Canopy.y_offset()),
                blade: Vec4::new(
                    tier.height_m,
                    tier.width_m,
                    tier.patch_m / tier.slots as f32 * JITTER_FRAC,
                    tier.patch_m,
                ),
                wind: Vec4::new(1.0, 0.0, CALM_SWAY, 1.0),
                range: Vec4::new(tier.retire.x, tier.retire.y, 0.0, 1.0),
                tint: blade_tint(),
                clump: Vec4::new(CLUMP_M, DENSITY_LOW, DENSITY_HIGH, CLUMP_FAN),
                range2: Vec4::new(tier.arrive.x, tier.arrive.y, 0.0, 0.0),
                mask: empty.clone(),
            },
        });
        let offsets = pool_offsets(&tier);
        patches.pools.push(TierPool {
            tier,
            mesh: meshes.add(lattice_mesh(&tier)),
            material,
            patches: Vec::new(),
            offsets,
            centre: None,
        });
    }
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
    // the pale sheet this used to stand on — and the *ratio* between the two is a
    // constant here, so that much is judgeable without trusting the render. How
    // the pair actually lands once lit is not: colour and exposure are not
    // evidence on the container's software rasteriser, so the final value belongs
    // on a workstation (`docs/notes/headless-shots-software-renderer.md`).
    //
    // These three multipliers are the whole reason the green family can be
    // repainted without a second edit here: the blade is defined as a *ratio* to
    // the fill, so a new fill carries the lawn with it. That held in both
    // directions on 2026-09-10 — the palette went saturated and came back, and
    // this line did not move either time.
    Vec4::new(r * 0.94, g * 0.99, b * 0.86, 0.45)
}

/// Build a tier's shared lattice as a Bevy mesh.
fn lattice_mesh(tier: &Tier) -> Mesh {
    use bevy::render::mesh::{Indices, PrimitiveTopology};

    let side = tier.slots;
    let lattice = turf::blade_lattice(side, tier.patch_m / side as f32);
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        // `RENDER_WORLD` only: nothing ever reads a blade slot back, and this
        // asset is otherwise duplicated in `Assets<Mesh>` for the life of the app.
        RenderAssetUsages::RENDER_WORLD,
    );
    // The lattice is placed with its centre on the patch origin, so a patch's
    // `Transform` is its middle and the `Aabb` above is symmetric.
    let half = tier.patch_m * 0.5;
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
fn mask_image(px: u32, data: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width: px,
            height: px,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
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
/// per-cell product is: this is ~150k point-in-polygon tests on the reduced tier
/// and ~410k on the full one, which is a frame or more on a phone. It is also
/// why the trigger has [`FIELD_MARGIN_M`] of slack — the answer is allowed to
/// take a while.
fn spawn_mask_job(
    patches: &GrassPatches,
    areas: &GrassAreas,
    terrain: &TerrainField,
    tunnels: &crate::systems::tunnels::Tunnels,
    cam: Vec2,
) {
    let cells = areas.snapshot();
    let ground = terrain.snapshot();
    let sink = patches.pending.clone();
    let (field_m, field_px) = field_dims();
    let origin = cam - Vec2::splat(field_m * 0.5);
    // Grass stands on the ground as the ground is drawn — filled over a lid —
    // not on the survey's old trench floor inside the tunnel under it.
    let lids = tunnels.lids_near(cam, field_m * 0.5 + cartopolis_geo::tunnels::DATUM_M);
    crate::platform::workers::submit(
        "grass",
        Box::new(move || {
            let placed: Vec<PlacedArea<'_>> = cells
                .iter()
                .map(|(_, o, t, a)| PlacedArea {
                    origin: [o.x, o.y],
                    tile_m: *t,
                    area: a.as_ref(),
                })
                .collect();
            let field =
                turf::mask_field(&placed, [origin.x, origin.y], field_m, field_px, &|x, z| {
                    lids.ground_at(x, z, &ground)
                });
            if let Ok(mut g) = sink.lock() {
                *g = Some(field);
            }
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pools_cover_their_radii_and_not_much_more() {
        for tier in tiers() {
            let offsets = pool_offsets(&tier);
            assert!(!offsets.is_empty());
            for o in &offsets {
                // Every patch must be reachable — a patch outside the fade end
                // is a mesh whose every blade has already retired…
                assert!(
                    patch_reach(*o, tier.patch_m) <= tier.retire.y + 1e-3,
                    "{o:?} is out of range"
                );
                // …and a far-ring patch entirely inside the arrive band is a
                // mesh whose every blade has yet to stand.
                assert!(
                    patch_far_reach(*o, tier.patch_m) >= tier.arrive.x - 2.0 - 1e-3,
                    "{o:?} is inside the arrive band"
                );
            }
        }
        // The near disc must be covered: the patch under the camera and its four
        // neighbours are the ones a standing avatar is actually looking at.
        let near = pool_offsets(&tiers()[0]);
        for o in [IVec2::ZERO, IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y] {
            assert!(near.contains(&o), "{o:?} missing from the near pool");
        }
    }

    #[test]
    fn every_ring_hands_over_where_the_one_inside_it_retires() {
        let tiers = tiers();
        // Walk the chain rather than naming tiers[0] and tiers[1]: this used to
        // check the one handover there was, which is exactly the assertion that
        // says nothing the day a third ring is added under it.
        for pair in tiers.windows(2) {
            let (inner, outer) = (&pair[0], &pair[1]);
            // The outer ring's arrive band must be exactly the inner one's
            // retire band: both draw a blade's cut from the same hash lane, so
            // equal bands are what conserves the expected blade count through
            // the handover — a gap is a bald ring around the camera, an overlap
            // a dense one.
            assert_eq!(inner.retire, outer.arrive);
            // …and each ring must actually reach past the one inside it, or the
            // LOD is a second pool under the first.
            assert!(outer.retire.y > inner.retire.y);
            // Its own retire must start beyond the handover, or blades begin
            // dying before they have finished arriving.
            assert!(outer.retire.x >= outer.arrive.y);
            // Density must fall outwards. A ring that is denser than the one
            // inside it is not a level of detail, and the handover would read as
            // a bright band rather than as nothing at all.
            let density = |t: &Tier| (t.slots * t.slots) as f32 / (t.patch_m * t.patch_m);
            assert!(density(outer) < density(inner));
        }
        // The mask has to cover the farthest blade with rebuild slack to spare —
        // an undersized field silently clips the outermost ring to the mask edge.
        let (field_m, _) = field_dims();
        assert!(field_m * 0.5 >= max_radius() + FIELD_MARGIN_M + 8.0);
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
        // `grass.wesl` declares locations 0, 1, 2 and 5 exactly. A missing
        // attribute does not fail to compile — it drops the matching shader-def,
        // and the pipeline is then built against a vertex struct the mesh does
        // not supply, which wgpu rejects at draw time.
        for tier in tiers() {
            let mesh = lattice_mesh(&tier);
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
                (tier.slots * tier.slots * turf::BLADE_VERTS) as usize
            );
        }
    }

    #[test]
    fn the_lattice_is_centred_on_its_patch() {
        // The patch `Transform` is its middle and the `Aabb` is symmetric about
        // it; a lattice laid from a corner instead would put every patch half a
        // patch off its own lattice slot, which reads as grass sliding under the
        // camera.
        for tier in tiers() {
            let mesh = lattice_mesh(&tier);
            let Some(bevy::render::mesh::VertexAttributeValues::Float32x3(p)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                panic!("positions are Float32x3");
            };
            let half = tier.patch_m * 0.5;
            for v in p {
                assert!(v[0] >= -half && v[0] <= half, "x {} off-patch", v[0]);
                assert!(v[2] >= -half && v[2] <= half, "z {} off-patch", v[2]);
            }
        }
    }
}
