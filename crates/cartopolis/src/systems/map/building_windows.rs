//! Windows on the 3DBAG buildings: the material extension and the clock that
//! drives it.
//!
//! `osm_buildings` has built real window quads for a long time, and over the
//! Netherlands none of them are ever drawn — that lane despawns its own detail
//! patch on every cell `lod22_covers()` reports, so the LoD2.2 meshes are the
//! only buildings there and they had no windows at all, by day or by night. This
//! is the missing half, written for the lane it has to serve.
//!
//! **Why a shader and not quads.** A z14 LoD2.2 cell is thousands of buildings
//! and ~240k triangles. Four floors of window quads on each is tens of megabytes
//! of extra mesh per cell, on the lane that already owns the largest assets in
//! the app and the documented device-loss churn. Nothing here is stored: the
//! pattern is computed per fragment from the wall's own normal and position, so
//! it costs no vertex, no upload and no residency, and it reaches every building
//! instead of a patch around the camera. `systems::grass` made this call for the
//! same reason and states it as a rule; `globe_ocean` is the other precedent.
//!
//! The consequence worth knowing is in `shaders/building_windows.wgsl`: with no
//! per-vertex anchor available the lattice has to *divide* `WORLD_CELL_EDGE`, or
//! the pattern shifts when big_space recentres. [`WINDOW_LATTICE`] documents the
//! arithmetic and [`tests`] pins it.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

use crate::resources::DevPanelState;

/// The LoD2.2 buildings' material: the same `StandardMaterial` the lane always
/// used, plus the window pattern.
pub type BuildingMaterial = ExtendedMaterial<StandardMaterial, BuildingWindows>;

/// Column pitch and floor pitch, in metres.
///
/// **Both must divide [`crate::grid::WORLD_CELL_EDGE`] exactly**, and this is not a taste:
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
const WINDOW_SIZE: Vec2 = Vec2::new(1.35, 1.55);

/// Emissive gain at full night.
///
/// **Measured down from 1.6 on 2026-08-26**, which lit Groningen to the horizon
/// like a stadium. Windows are a warm point in a dark street, not a light source;
/// bloom is on, and at the old value every facade in the city bloomed into a
/// single sheet. The capture that showed it is the reason this is a number and
/// not a guess.
const NIGHT_GAIN: f32 = 0.62;

/// How much a window darkens the facade by day, as a multiplier on the wall
/// colour.
///
/// Deliberately mild. The facades are pale cartographic tones and a hard black
/// grid over them is the loudest thing on the street — the same judgement
/// `furniture`'s palette makes about a saturated bench. It also means the layer
/// is visible in daylight at all, which is most of how it gets checked.
const DAY_TINT: f32 = 0.80;

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
pub struct BuildingWindows {
    /// x: column pitch (m). y: floor pitch (m). z: pane width. w: pane height.
    #[uniform(100)]
    pub lattice: Vec4,
    /// x: night factor, 0 by day → 1 after dusk. y: emissive gain at full night.
    /// z: the daylight darkening. w: the first sill's height.
    #[uniform(101)]
    pub glow: Vec4,
    /// x: occupancy block size (m). y: how many blocks it repeats over.
    /// z, w: the fraction of windows lit in the deadest and the liveliest block.
    #[uniform(103)]
    pub block: Vec4,
    /// rgb: the lit colour, linear. a unused.
    #[uniform(102)]
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
    #[uniform(104)]
    pub ghost: Vec4,
    /// The ghost ring's bounding box in render-space XZ — `(min x, min z, max x,
    /// max z)`. The broad phase, and not optional: without it every wall fragment
    /// in the city walks the polygon while anything is ghosted.
    #[uniform(105)]
    pub ghost_bounds: Vec4,
    /// The ghost ring in render-space XZ, two points per element (`xy` then `zw`).
    ///
    /// **Render space, so it is rewritten every frame** — under `big_space` the
    /// origin jumps a whole `WORLD_CELL_EDGE` as the camera crosses a grid cell,
    /// which is the trap this file's `WINDOW_LATTICE` note is entirely about. A
    /// ring is 32 floats and there is one material; paying for it once a frame is
    /// cheaper than any arrangement that makes the shader solve it.
    #[uniform(106)]
    pub ghost_ring: [Vec4; GHOST_RING_VEC4S],
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

/// How lit the windows are at a given sun elevation, 0 by day → 1 after dusk.
///
/// Split out as a plain function of the elevation so the ramp is testable and so
/// it can be stated once: it is **the same ramp** `sky::update_window_glow` runs
/// for the OSM lane. The two lanes meet at every 3DBAG coverage edge, and windows
/// that came up at different times either side of one would draw that edge on the
/// map at dusk.
#[must_use]
pub fn night_factor(sun_elevation: f32) -> f32 {
    ((0.18 - sun_elevation) / 0.33).clamp(0.0, 1.0)
}

/// Drive the night factor from the clock.
///
/// **Deliberately not gated on `dev.is_changed()`**, which is what every other
/// consumer of the time of day does and what this did first. The clock settles
/// during startup; the shared material is created *lazily*, on the first LoD2.2
/// cell that spawns, which is seconds later. Gated, the material was therefore
/// born holding the default glow of zero and nothing ever wrote to it again — a
/// whole city that stays dark at midnight, with no error anywhere and a healthy
/// building count. `sky::update_window_glow` avoids the same trap for the other
/// lane with a second `Added<WindowGlow>` query; a shared uniform has no
/// equivalent, so it compares instead.
///
/// The comparison is what keeps it cheap: there is exactly one material, and
/// `Assets::get_mut` is only reached on the frames the answer actually moved.
/// Touching it every frame would re-upload the uniform and rebuild the bind group
/// for nothing.
pub fn update_building_windows(
    dev: Res<DevPanelState>,
    mut materials: ResMut<Assets<BuildingMaterial>>,
) {
    let elevation = crate::utils::sun::sun_direction(dev.time_of_day, dev.sun_declination_rad).y;
    let factor = night_factor(elevation);
    let stale: Vec<_> = materials
        .iter()
        .filter(|(_, m)| (m.extension.glow.x - factor).abs() > 1e-4)
        .map(|(id, _)| id)
        .collect();
    for id in stale {
        if let Some(mut m) = materials.get_mut(id) {
            m.extension.glow.x = factor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::WORLD_CELL_EDGE;

    /// The one thing about this layer that a picture will not tell you and a
    /// flight across the city will: the lattice has to divide the floating
    /// origin's cell edge, or every window in view changes when big_space
    /// recentres. See the module docs.
    #[test]
    fn the_lattice_divides_the_floating_origin_cell() {
        for pitch in [WINDOW_LATTICE.x, WINDOW_LATTICE.y] {
            let steps = WORLD_CELL_EDGE / pitch;
            assert_eq!(
                steps,
                steps.round(),
                "a {pitch} m pitch does not divide the {WORLD_CELL_EDGE} m cell edge"
            );
            // And the shader hashes the index modulo 32, so the *index* jump has
            // to be a whole number of periods too.
            const HASH_PERIOD: f32 = 32.0;
            let periods = steps / HASH_PERIOD;
            assert_eq!(
                periods,
                periods.round(),
                "a {pitch} m pitch steps {steps} indices, which is not a whole \
                 number of {HASH_PERIOD}-window hash periods"
            );
        }
    }

    /// The occupancy lattice has to survive the floating origin too, on exactly
    /// the same terms as the window lattice — a block that changes liveliness
    /// when you fly 10 km is the same bug one step coarser.
    #[test]
    fn the_occupancy_block_survives_the_floating_origin() {
        let steps = WORLD_CELL_EDGE / BLOCK_M;
        assert_eq!(
            steps,
            steps.round(),
            "{BLOCK_M} m does not divide the cell edge"
        );
        let periods = steps / BLOCK_PERIOD;
        assert_eq!(
            periods,
            periods.round(),
            "{steps} block steps is not a whole number of {BLOCK_PERIOD}-block periods"
        );
    }

    /// The panes have to be smaller than their cells, or the "windows" tile the
    /// whole wall and there is no facade left between them.
    #[test]
    fn a_pane_fits_inside_its_cell() {
        assert!(
            WINDOW_SIZE.x < WINDOW_LATTICE.x,
            "panes touch across columns"
        );
        assert!(
            WINDOW_SIZE.y < WINDOW_LATTICE.y,
            "panes touch across floors"
        );
    }

    /// The ramp is the same one the OSM lane runs, and the shape matters: full
    /// dark well before midnight, nothing at all at noon, and a gradual middle
    /// rather than a switch at dusk.
    #[test]
    fn windows_come_up_over_dusk_and_not_at_a_step() {
        assert_eq!(night_factor(1.0), 0.0, "noon: nothing lit");
        assert_eq!(night_factor(-0.5), 1.0, "deep night: fully lit");
        let dusk = night_factor(0.0);
        assert!(
            dusk > 0.4 && dusk < 0.7,
            "the sun on the horizon should be mid-ramp, got {dusk}"
        );
        // Monotone, so nothing brightens as the sun rises.
        let mut last = f32::MAX;
        for i in 0..=20 {
            let e = 0.4 - i as f32 * 0.05;
            let f = night_factor(e);
            assert!(f >= last - 1e-6 || last == f32::MAX, "ramp went backwards");
            last = f;
        }
    }
}
