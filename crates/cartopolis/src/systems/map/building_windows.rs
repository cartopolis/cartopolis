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

/// What the facade shader is told about one building, packed into the vertex
/// colour's alpha ([`pack_facade`]).
///
/// The shader can see a wall's position and normal and nothing else, and the
/// module note says why a vertex attribute is the wrong way to tell it more.
/// The alpha channel was already carrying one per-building number — the night
/// occupancy — for free, because the material is opaque; an `f32` has room for
/// the rest beside it, exactly:
///
/// `alpha = occupancy + 4 × code`, occupancy quantised to sixteenths in [0, 4),
/// and `code = shop | era << 1 | eave << 4`, eave in 20 cm steps (0 = unknown,
/// up to 102 m). The largest alpha is 2¹⁵, and a sixteenth of that is still
/// exact in an `f32`'s 24 bits. **Code 0 is "nothing known"**, and `1.0` —
/// what every building off the BAG, and the OSM lane, has always carried —
/// decodes to occupancy 1 and code 0, which is exactly the old look.
///
/// The [`Condition`] rides in the occupancy's unused top: the shader never lit
/// past an occupancy of 2 and `night_lit_scale` tops out under 1.5, so a kept
/// building's occupancy is clamped to 2.5 and 3 and 3.5 say "vacant" and
/// "condemned" — no new bit, and no larger alpha. Both sit a quarter clear of
/// their neighbours, so the GPU's inexact interpolation of three equal values
/// cannot move one into the other. The decode adds 1/32 before taking the
/// floor for the same reason: an occupancy of exactly 0 interpolated a hair
/// low would otherwise borrow from the code and read as a different building.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Facade {
    /// `osm_buildings::night_lit_scale`.
    pub occupancy: f32,
    /// The register lists a shop in it: glass on the ground floor.
    pub shop: bool,
    /// [`era`] of its construction year, 0 when unknown.
    pub era: u8,
    /// Height of its eaves above its own base, when the shell was measured.
    pub eave_m: Option<f32>,
    /// Whether anybody is looking after it ([`condition`]).
    pub condition: Condition,
}

/// What the register says about a building's upkeep, as the facade shows it.
///
/// A vacant building is dark at night, its ground floor boarded and its walls
/// grimier; a condemned one (a demolition permit granted) more so. Both are the
/// register's word, never a guess from anything else.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Condition {
    #[default]
    Kept,
    /// *Buiten gebruik*: standing, and out of use.
    Vacant,
    /// *Sloopvergunning verleend*: waiting to come down.
    Condemned,
}

/// The facade's [`Condition`] for a register status.
#[must_use]
pub fn condition(status: crate::systems::building_facts::BuildingStatus) -> Condition {
    use crate::systems::building_facts::BuildingStatus as S;
    match status {
        S::OutOfUse => Condition::Vacant,
        S::DemolitionPermit => Condition::Condemned,
        _ => Condition::Kept,
    }
}

/// The shader's window style for a construction year: 1 before 1920 (tall,
/// narrow sashes), 2 to 1960 (squarer), 3 to 1990 (horizontal bands), 4 since
/// (large panes). 0 is unknown and draws the old uniform lattice.
#[must_use]
pub fn era(year: Option<u16>) -> u8 {
    match year {
        None => 0,
        Some(y) if y < 1920 => 1,
        Some(y) if y < 1960 => 2,
        Some(y) if y < 1990 => 3,
        Some(_) => 4,
    }
}

const EAVE_STEP_M: f32 = 0.2;
const EAVE_MAX_STEPS: u32 = 511;
/// The occupancy values that carry a [`Condition`]. See [`Facade`].
const VACANT_OCCUPANCY: f32 = 3.0;
const CONDEMNED_OCCUPANCY: f32 = 3.5;

/// See [`Facade`].
#[must_use]
pub fn pack_facade(f: Facade) -> f32 {
    let occupancy = match f.condition {
        Condition::Kept => (f.occupancy.clamp(0.0, 2.5) * 16.0).round() / 16.0,
        Condition::Vacant => VACANT_OCCUPANCY,
        Condition::Condemned => CONDEMNED_OCCUPANCY,
    };
    let eave = f.eave_m.map_or(0, |e| {
        ((e / EAVE_STEP_M).round().max(1.0) as u32).min(EAVE_MAX_STEPS)
    });
    let code = u32::from(f.shop) | (u32::from(f.era.min(7)) << 1) | (eave << 4);
    occupancy + 4.0 * code as f32
}

/// The shader's decode, mirrored for the tests that pin the two together.
#[cfg(test)]
fn unpack_facade(a: f32) -> Facade {
    let code = ((a + 1.0 / 32.0) / 4.0).floor();
    let raw = (a - 4.0 * code).max(0.0);
    let code = code as u32;
    let eave = code >> 4;
    let (occupancy, condition) = if raw > 3.25 {
        (0.0, Condition::Condemned)
    } else if raw > 2.75 {
        (0.0, Condition::Vacant)
    } else {
        (raw, Condition::Kept)
    };
    Facade {
        occupancy,
        shop: code & 1 == 1,
        era: ((code >> 1) & 7) as u8,
        eave_m: (eave > 0).then(|| eave as f32 * EAVE_STEP_M),
        condition,
    }
}

// ─── Wall faces ──────────────────────────────────────────────────────────────

/// Window bay width by [`era`], metres: the target the real width is rounded
/// to so that a whole number of bays fills the wall.
const BAY_M: [f32; 5] = [2.5, 2.1, 2.6, 2.5, 3.0];
/// A wall face shorter than this carries no windows — a chimney breast, the
/// side of a dormer, a jog in a facade.
const MIN_WINDOWED_M: f32 = 1.5;
/// A face this short cannot take a door with its surround.
const MIN_DOOR_FACE_M: f32 = 1.8;
/// How far from a wall face an entrance point may lie and still be its door.
/// The register's points are "at or near" the entrance.
const DOOR_REACH_M: f32 = 2.5;
/// Door positions are carried in steps of this, and snapped to them before
/// being drawn, so the shader's gap in the windows and the drawn door agree.
pub const DOOR_STEP_M: f32 = 0.2;
/// Half the width the shader keeps clear of windows around a door: the leaf,
/// its surround, and a margin. The shader's `DOOR_CLEAR_M` is the same number.
pub const DOOR_CLEAR_M: f32 = 1.0;

/// One flat wall of a building: every wall triangle that shares its plane.
#[derive(Clone, Debug)]
pub struct WallFace {
    /// Outward horizontal normal (from the triangles' winding — the shell is
    /// back-face culled, so that winding faces out).
    pub outward: Vec2,
    /// Distance of the plane from the origin along `outward`.
    pub d: f32,
    /// Along the face, `tangent · p`: where it starts, and how long it is.
    pub u0: f32,
    pub len: f32,
    /// Lowest point of the face above the building's base.
    pub low: f32,
    /// Highest point of the face above the building's base.
    pub high: f32,
    /// Where along the face its door stands, snapped to [`DOOR_STEP_M`].
    pub door: Option<f32>,
    bay: f32,
}

impl WallFace {
    #[must_use]
    pub fn tangent(&self) -> Vec2 {
        Vec2::new(-self.outward.y, self.outward.x)
    }

    /// The UV0 its vertices carry: `x` metres along the face from its start,
    /// `y` the packed layout (see [`WallFaces`]).
    #[must_use]
    pub fn uv(&self, p: Vec2) -> [f32; 2] {
        let bay_cm = if self.len < MIN_WINDOWED_M {
            1
        } else {
            ((self.bay * 100.0).round() as u32).clamp(2, 511)
        };
        let door = self
            .door
            .map_or(0, |u| ((u / DOOR_STEP_M).round() as u32).clamp(1, 2047));
        [
            self.tangent().dot(p) - self.u0,
            (bay_cm + 512 * door) as f32,
        ]
    }

    /// The door's centre on the wall plane, in the same frame as the points.
    #[must_use]
    pub fn door_at(&self) -> Option<Vec2> {
        self.door
            .map(|u| self.tangent() * (self.u0 + u) + self.outward * self.d)
    }
}

/// The UV0 of a wall that must carry no painted windows: no position along a
/// face, and the 1 cm bay [`WallFace::uv`] gives a face too short for one. An
/// imported model (`systems::lod3`) is the case — its windows, if it has any,
/// are its own geometry, and a stadium's stands painted as office floors read
/// as a building, not as seating.
pub const WINDOWLESS_UV: [f32; 2] = [0.0, 1.0];

/// A building's walls grouped into flat faces, for the facade shader.
///
/// The shader could see a wall's normal and position and nothing else, so it
/// laid windows on one lattice for the whole world: a window could straddle a
/// building's corner or a party wall, and nothing knew where the door was. Each
/// face now carries its own coordinate along itself (UV0.x, from its own
/// corner), and in UV0.y an integer (`bay_cm + 512 × door_step`): the bay width
/// that fits a whole number of windows into it, and where along it its door
/// stands (0 for none). Kept under 2²⁰ so that the GPU's interpolation of three
/// equal values, which is not exact, still rounds back to the same integer.
/// Door parts use negative UV0.y (see `lod22`'s door); 0 is "no layout" and
/// draws the old world lattice, which is what the OSM lane still gets.
#[derive(Clone, Debug, Default)]
pub struct WallFaces {
    pub faces: Vec<WallFace>,
    /// Which face each triangle of the input belongs to.
    pub of_triangle: Vec<Option<u32>>,
}

impl WallFaces {
    /// Group the wall triangles of one building. `pts` are its vertices with `y`
    /// the height above its base; `tris` its triangles with the roof flag last.
    #[must_use]
    pub fn new(pts: &[Vec3], tris: &[(usize, usize, usize, u8)], era: u8) -> Self {
        use std::collections::HashMap;
        let bay_target = BAY_M[usize::from(era.min(4))];
        let mut key_to_face: HashMap<(i32, i32), u32> = HashMap::new();
        let mut faces: Vec<WallFace> = Vec::new();
        let mut of_triangle = Vec::with_capacity(tris.len());
        for &(i, j, k, roof) in tris {
            let (Some(&a), Some(&b), Some(&c)) = (pts.get(i), pts.get(j), pts.get(k)) else {
                of_triangle.push(None);
                continue;
            };
            let n = (b - a).cross(c - a).normalize_or_zero();
            let h = Vec2::new(n.x, n.z);
            if roof != 0 || n.y.abs() >= 0.42 || h.length() < 1e-3 {
                of_triangle.push(None);
                continue;
            }
            let outward = h.normalize();
            let d = outward.dot(Vec2::new(a.x, a.z));
            let angle = outward.y.atan2(outward.x).to_degrees();
            let key = ((angle / 4.0).round() as i32, (d / 0.3).round() as i32);
            let idx = *key_to_face.entry(key).or_insert_with(|| {
                faces.push(WallFace {
                    outward,
                    d,
                    u0: f32::MAX,
                    len: f32::MIN,
                    low: f32::MAX,
                    high: f32::MIN,
                    door: None,
                    bay: bay_target,
                });
                (faces.len() - 1) as u32
            });
            let face = &mut faces[idx as usize];
            let t = face.tangent();
            for p in [a, b, c] {
                let u = t.dot(Vec2::new(p.x, p.z));
                // `len` holds the far end until the pass below.
                face.u0 = face.u0.min(u);
                face.len = face.len.max(u);
                face.low = face.low.min(p.y);
                face.high = face.high.max(p.y);
            }
            of_triangle.push(Some(idx));
        }
        for face in &mut faces {
            face.len -= face.u0;
            let bays = (face.len / bay_target).round().max(1.0);
            face.bay = face.len / bays;
        }
        Self { faces, of_triangle }
    }

    /// Put a door at an entrance point: on the nearest face standing on the
    /// ground that is long enough for one, at the point's position along it,
    /// kept clear of the face's ends and snapped to [`DOOR_STEP_M`].
    ///
    /// **One door a face.** The register gives a point per *address*, and the
    /// flats over a row of shops share one street door between them: drawn one
    /// door each, the Herestraat had a front door every few metres. A point
    /// whose face already has its door is that door, not a second one on the
    /// next wall along — which is where the first version put them.
    pub fn place_door(&mut self, p: Vec2) -> DoorPlacement {
        let Some((i, _)) = self
            .faces
            .iter()
            .enumerate()
            .filter(|(_, f)| f.len >= MIN_DOOR_FACE_M && f.low < 0.5)
            .map(|(i, f)| {
                let u = f.tangent().dot(p) - f.u0;
                let off_end = (-u).max(u - f.len).max(0.0);
                (i, (f.outward.dot(p) - f.d).abs() + off_end)
            })
            .filter(|&(_, dist)| dist < DOOR_REACH_M)
            .min_by(|a, b| a.1.total_cmp(&b.1))
        else {
            return DoorPlacement::Nowhere;
        };
        let face = &mut self.faces[i];
        if face.door.is_some() {
            return DoorPlacement::Merged;
        }
        let margin = (DOOR_CLEAR_M * 0.8).min(face.len * 0.5);
        let u = (face.tangent().dot(p) - face.u0).clamp(margin, face.len - margin);
        face.door = Some((u / DOOR_STEP_M).round().max(1.0) * DOOR_STEP_M);
        match face.door_at() {
            Some(at) => DoorPlacement::On(i, at),
            None => DoorPlacement::Nowhere,
        }
    }
}

/// Where [`WallFaces::place_door`] put an entrance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DoorPlacement {
    /// On this face, centred here.
    On(usize, Vec2),
    /// On a face that already had its door: the same entrance.
    Merged,
    /// No wall face near enough to carry it.
    Nowhere,
}

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
    use super::{DoorPlacement, WallFaces, DOOR_STEP_M};
    use bevy::math::{Vec2, Vec3};

    /// A 10 × 6 m box, 7 m tall, as two triangles a wall and a flat roof.
    fn box_building() -> (Vec<Vec3>, Vec<(usize, usize, usize, u8)>) {
        let corners = [
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 6.0),
            Vec2::new(0.0, 6.0),
        ];
        let mut pts = Vec::new();
        for c in corners {
            pts.push(Vec3::new(c.x, 0.0, c.y));
            pts.push(Vec3::new(c.x, 7.0, c.y));
        }
        let mut tris = Vec::new();
        for i in 0..4usize {
            let (a0, a1) = (2 * i, 2 * i + 1);
            let (b0, b1) = (2 * ((i + 1) % 4), 2 * ((i + 1) % 4) + 1);
            // Wound so the normal faces away from the box's middle.
            tris.push((a0, a1, b0, 0));
            tris.push((b0, a1, b1, 0));
        }
        tris.push((1, 3, 5, 1));
        (pts, tris)
    }

    #[test]
    fn a_box_has_four_faces_facing_out_with_whole_bays() {
        let (pts, tris) = box_building();
        let walls = WallFaces::new(&pts, &tris, 0);
        assert_eq!(walls.faces.len(), 4);
        assert_eq!(walls.of_triangle.iter().flatten().count(), 8);
        let middle = Vec2::new(5.0, 3.0);
        for f in &walls.faces {
            let centre = f.tangent() * (f.u0 + f.len / 2.0) + f.outward * f.d;
            assert!(f.outward.dot(centre - middle) > 0.0, "a face points inward");
            let bays = f.len / f.bay;
            assert!(
                (bays - bays.round()).abs() < 1e-3,
                "{} m in {} m bays",
                f.len,
                f.bay
            );
            assert!(f.len == 10.0 || f.len == 6.0);
        }
    }

    #[test]
    fn a_door_lands_on_its_wall_clear_of_the_corner_and_on_the_step() {
        let (pts, tris) = box_building();
        let mut walls = WallFaces::new(&pts, &tris, 1);
        // Just outside the long south wall (z = 0), right by its corner.
        let DoorPlacement::On(i, at) = walls.place_door(Vec2::new(0.3, -0.8)) else {
            panic!("no door placed");
        };
        let face = &walls.faces[i];
        assert!(at.y.abs() < 1e-4, "the door is off its wall: {at}");
        let u = face.door.unwrap();
        assert!(u >= 0.8 && u <= face.len - 0.8, "{u}");
        assert!(((u / DOOR_STEP_M) - (u / DOOR_STEP_M).round()).abs() < 1e-4);
        // …and its UV carries it, round-trippable through the shader's decode.
        let code = face.uv(at)[1] as u32;
        assert_eq!(code / 512, (u / DOOR_STEP_M).round() as u32);
        assert!((f64::from(code % 512) / 100.0 - f64::from(face.bay)).abs() < 0.01);
        assert!(face.uv(at)[1] < (1u32 << 20) as f32);
        // A second address at the same entrance is the same door.
        assert_eq!(
            walls.place_door(Vec2::new(1.4, -0.5)),
            DoorPlacement::Merged
        );
        // …and a point in the middle of nowhere has no wall to be on.
        assert_eq!(
            walls.place_door(Vec2::new(50.0, 50.0)),
            DoorPlacement::Nowhere
        );
    }

    #[test]
    fn nothing_known_packs_to_the_alpha_every_building_used_to_carry() {
        let none = super::Facade {
            occupancy: 1.0,
            ..Default::default()
        };
        assert_eq!(super::pack_facade(none), 1.0);
        assert_eq!(super::unpack_facade(1.0), none);
    }

    /// A vacant or condemned building survives the alpha channel on the largest
    /// code there is, and so does an occupancy of 0 a hair under its packing —
    /// the GPU interpolates three equal values inexactly.
    #[test]
    fn a_condition_survives_the_alpha_channel() {
        use super::Condition;
        for condition in [Condition::Kept, Condition::Vacant, Condition::Condemned] {
            for occupancy in [0.0, 1.4375] {
                let f = super::Facade {
                    occupancy,
                    shop: true,
                    era: 4,
                    eave_m: Some(102.2),
                    condition,
                };
                let a = super::pack_facade(f);
                for wobble in [-0.02, 0.0, 0.02] {
                    let back = super::unpack_facade(a + wobble);
                    assert_eq!(back.condition, condition, "{f:?} at {wobble}");
                    assert_eq!((back.shop, back.era), (true, 4), "{f:?} at {wobble}");
                    if condition == Condition::Kept {
                        assert!((back.occupancy - occupancy).abs() < 0.05, "{back:?}");
                    } else {
                        assert_eq!(back.occupancy, 0.0, "a vacant building is dark");
                    }
                }
            }
        }
    }

    #[test]
    fn only_the_registers_word_makes_a_building_vacant() {
        use super::{condition, Condition};
        use crate::systems::building_facts::BuildingStatus as S;
        assert_eq!(condition(S::OutOfUse), Condition::Vacant);
        assert_eq!(condition(S::DemolitionPermit), Condition::Condemned);
        for s in [S::InUse, S::Renovation, S::UnderConstruction, S::Unknown] {
            assert_eq!(condition(s), Condition::Kept);
        }
    }

    #[test]
    fn a_facade_survives_the_alpha_channel() {
        for f in [
            super::Facade {
                occupancy: 1.15,
                shop: true,
                era: 1,
                eave_m: Some(9.4),
                condition: super::Condition::Kept,
            },
            super::Facade {
                occupancy: 0.25,
                shop: false,
                era: 4,
                eave_m: Some(102.2),
                condition: super::Condition::Kept,
            },
        ] {
            let back = super::unpack_facade(super::pack_facade(f));
            assert!(
                (back.occupancy - f.occupancy).abs() <= 1.0 / 32.0,
                "{back:?}"
            );
            assert_eq!((back.shop, back.era), (f.shop, f.era));
            assert!(
                (back.eave_m.unwrap() - f.eave_m.unwrap()).abs() <= 0.1,
                "{back:?}"
            );
        }
    }

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
