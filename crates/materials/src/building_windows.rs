//! The LoD2.2 buildings' material: the window pattern on their walls, and the
//! constants its defaults are built from. The facade packing and the clock that
//! drives it are `cartopolis::systems::building_windows`.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

/// The LoD2.2 buildings' material: the same `StandardMaterial` the lane always
/// used, plus the window pattern.
pub type BuildingMaterial = ExtendedMaterial<StandardMaterial, BuildingWindows>;

/// Column pitch and floor pitch, in metres.
///
/// **Both must divide `cartopolis::grid::WORLD_CELL_EDGE` exactly**, and this is not a taste:
/// the shader reads `world_position`, which jumps by a whole cell edge when
/// big_space recentres the floating origin, and a lattice that does not divide
/// the jump slides the whole city's windows sideways as you fly. 10,000 / 2.5 =
/// 4,000 and 10,000 / 3.125 = 3,200, both whole, and both divisible by the
/// shader's `HASH_PERIOD` of 32 — which is the other half of the same
/// requirement, since the per-window hash is taken on the index.
///
/// 2.5 m between windows and 3.125 m between floors are both close enough to the
/// real thing (`osm_buildings::STOREY_HEIGHT` is 3.0) that nobody will measure
/// the difference off a facade.
pub const WINDOW_LATTICE: Vec2 = Vec2::new(2.5, 3.125);

/// Pane size, metres. Narrower and shorter than the lattice, so the wall shows
/// between the windows rather than the windows being the wall.
pub const WINDOW_SIZE: Vec2 = Vec2::new(1.35, 1.55);

/// Emissive gain at full night.
///
/// **Measured down from 1.6 on 2026-08-26**, which lit Groningen to the horizon
/// like a stadium. Windows are a warm point in a dark street, not a light source;
/// bloom is on, and at the old value every facade in the city bloomed into a
/// single sheet. The capture that showed it is the reason this is a number and
/// not a guess.
const NIGHT_GAIN: f32 = 0.62;

/// A window's colour by day, as a multiplier on the wall's own (untextured)
/// colour.
///
/// It was 0.80, deliberately mild, while the facades were flat cartographic
/// tones and a dark grid was the loudest thing on the street. Once the walls
/// carried brick and render (2026-09-30) a pane that mild read as a bricked-up
/// opening at street level; glass is dark, and what lightens it is the sky it
/// reflects, which the glass roughness and reflectance supply.
const DAY_TINT: f32 = 0.35;

/// No windows below this height above the world's zero.
///
/// A fragment this low is a plinth, a garden wall or the side of a shed, and
/// lighting those reads as a mistake rather than as a window. It is measured
/// from the world's ground rather than each building's base for the reason the
/// shader documents — it cannot see the base.
const FIRST_SILL_M: f32 = 2.2;

/// The lit colour, linear RGB. Warm, and the same hue
/// `sky::update_window_glow` uses for the other lane, so a street that straddles
/// the 3DBAG coverage edge does not change colour across it.
const LIT_COLOR: Vec3 = Vec3::new(1.0, 0.72, 0.34);

/// Size of the block the *occupancy* hash is taken on, in metres, and how many
/// blocks it repeats over.
///
/// A per-window hash alone gives every building in the city the same fraction of
/// lit windows, which is what made the first night capture read as decoration
/// rather than as a city: real streets have whole dark buildings standing between
/// the lit ones — an office after hours, an empty flat, a warehouse. This is a
/// second, coarser hash on roughly one building's footprint that decides how
/// *lively* the block is, and the window hash is then tested against it.
///
/// 25 m divides `WORLD_CELL_EDGE` into 400 steps and 400 is 25 periods of 16, so
/// it survives a floating-origin jump on the same terms the window lattice does —
/// see [`WINDOW_LATTICE`], and do not change either number without redoing that
/// arithmetic.
pub const BLOCK_M: f32 = 25.0;
/// How many blocks the occupancy hash repeats over. See [`BLOCK_M`].
pub const BLOCK_PERIOD: f32 = 16.0;

/// Uniforms for `shaders/building_windows.wgsl`.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
// One uniform buffer, not one per field: see the shader's `Facade` for the
// browser limit seven of them broke.
#[uniform(100, BuildingWindowsGpu)]
pub struct BuildingWindows {
    /// x: column pitch (m). y: floor pitch (m). z: pane width. w: pane height.
    pub lattice: Vec4,
    /// x: night factor, 0 by day → 1 after dusk. y: emissive gain at full night.
    /// z: the daylight darkening. w: the first sill's height.
    pub glow: Vec4,
    /// x: occupancy block size (m). y: how many blocks it repeats over.
    /// z, w: the fraction of windows lit in the deadest and the liveliest block.
    pub block: Vec4,
    /// rgb: the lit colour, linear. a unused.
    pub lit: Vec4,
    /// x: how many of [`Self::ghost_ring`]'s points are live, 0 for nothing
    /// ghosted. y, z, w unused.
    ///
    /// The ghosted building is the one the avatar is standing in: its **walls**
    /// are discarded so you can see the room and the avatar behind them, and
    /// `systems::building_select` draws a translucent shell where they were. The
    /// roof is kept — see the shader.
    ///
    /// A uniform rather than a per-vertex key for the reason this whole module
    /// exists: four bytes a vertex on the largest geometry in the app, carried
    /// forever, to serve a state that is off almost all of the time.
    pub ghost: Vec4,
    /// The ghost ring's bounding box in render-space XZ — `(min x, min z, max x,
    /// max z)`. The broad phase, and not optional: without it every wall fragment
    /// in the city walks the polygon while anything is ghosted.
    pub ghost_bounds: Vec4,
    /// The ghost ring in render-space XZ, two points per element (`xy` then `zw`).
    ///
    /// **Render space, so it is rewritten every frame** — under `big_space` the
    /// origin jumps a whole `WORLD_CELL_EDGE` as the camera crosses a grid cell,
    /// which is the trap this file's `WINDOW_LATTICE` note is entirely about. A
    /// ring is 32 floats and there is one material; paying for it once a frame is
    /// cheaper than any arrangement that makes the shader solve it.
    pub ghost_ring: [Vec4; GHOST_RING_VEC4S],
}

/// [`BuildingWindows`]' uniforms as `building_windows.wgsl`'s `Facade` lays
/// them out, in that order.
#[derive(ShaderType, Clone, Copy, Debug)]
pub struct BuildingWindowsGpu {
    pub lattice: Vec4,
    pub glow: Vec4,
    pub lit: Vec4,
    pub block: Vec4,
    pub ghost: Vec4,
    pub ghost_bounds: Vec4,
    pub ghost_ring: [Vec4; GHOST_RING_VEC4S],
}

/// What `#[uniform(100, BuildingWindowsGpu)]` binds.
impl From<&BuildingWindows> for BuildingWindowsGpu {
    fn from(m: &BuildingWindows) -> Self {
        Self {
            lattice: m.lattice,
            glow: m.glow,
            lit: m.lit,
            block: m.block,
            ghost: m.ghost,
            ghost_bounds: m.ghost_bounds,
            ghost_ring: m.ghost_ring,
        }
    }
}

/// How many `vec4`s the ghost ring is packed into — two points each, so 32
/// points. See [`BuildingWindows::ghost_ring`]; the shader's `GHOST_MAX_POINTS`
/// is the same number and they must agree.
pub const GHOST_RING_VEC4S: usize = 16;
/// Ring points the shader can hold. A BAG outline simplified at 10 cm is about
/// nine of them.
pub const GHOST_MAX_POINTS: usize = GHOST_RING_VEC4S * 2;

impl Default for BuildingWindows {
    fn default() -> Self {
        Self {
            lattice: Vec4::new(
                WINDOW_LATTICE.x,
                WINDOW_LATTICE.y,
                WINDOW_SIZE.x,
                WINDOW_SIZE.y,
            ),
            glow: Vec4::new(0.0, NIGHT_GAIN, DAY_TINT, FIRST_SILL_M),
            lit: Vec4::new(LIT_COLOR.x, LIT_COLOR.y, LIT_COLOR.z, 0.0),
            // A dead block lights one window in twelve — a stairwell, somebody
            // working late — and the liveliest lights half. Neither end is zero
            // or one: a wholly black building reads as missing, and a wholly lit
            // one is the thing this exists to stop.
            block: Vec4::new(BLOCK_M, BLOCK_PERIOD, 0.08, 0.5),
            ghost: Vec4::ZERO,
            ghost_bounds: Vec4::ZERO,
            ghost_ring: [Vec4::ZERO; GHOST_RING_VEC4S],
        }
    }
}

impl MaterialExtension for BuildingWindows {
    fn fragment_shader() -> ShaderRef {
        "shaders/building_windows.wgsl".into()
    }
}
