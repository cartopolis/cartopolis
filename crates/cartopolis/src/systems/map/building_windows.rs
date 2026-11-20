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
//! The consequence worth knowing is in `shaders/building_windows.wesl`: with no
//! per-vertex anchor available the lattice has to *divide* `WORLD_CELL_EDGE`, or
//! the pattern shifts when big_space recentres.
//! [`WINDOW_LATTICE`](cartopolis_materials::building_windows::WINDOW_LATTICE) documents the
//! arithmetic and [`tests`] pins it.

use bevy::prelude::*;

use crate::resources::DevPanelState;

// The material, its uniforms and the constants its defaults are built from live
// in `cartopolis_materials`, so that Bevy's per-material plugin code is generated
// there rather than in this crate.
pub use cartopolis_materials::building_windows::{
    BuildingMaterial, BuildingWindows, ATTRIBUTE_FACADE_CODE, GHOST_MAX_POINTS, GHOST_RING_VEC4S,
};
// The lattice arithmetic the tests below pin.
#[cfg(test)]
use cartopolis_materials::building_windows::{BLOCK_M, BLOCK_PERIOD, WINDOW_LATTICE, WINDOW_SIZE};

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
/// and `code = shop | era << 1 | eave << 4 | material << 13`, eave in 20 cm steps
/// (0 = unknown, up to 102 m) and material two bits (0 = unmeasured). The
/// largest alpha is 2¹⁷, and a sixteenth of that is still exact in an `f32`'s
/// 24 bits. **Code 0 is "nothing known"**, and `1.0` —
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
    /// What atlas measured its walls to be made of ([`wall_material`]), 0 when
    /// nothing was measured and the shader reads the material off the colour.
    pub material: u8,
}

/// The shader's code for a measured roof covering
/// (`building_windows.wesl::ROOF_*`), 0 for "not measured".
#[must_use]
pub fn roof_covering(k: cartopolis_geo::formats::facades::Covering) -> u8 {
    use cartopolis_geo::formats::facades::Covering as C;
    match k {
        C::Unknown => 0,
        C::Pantile => 1,
        C::Glazed => 2,
        C::Concrete => 3,
        C::Slate => 4,
        C::Metal => 5,
        C::Solar => 6,
        C::Bitumen => 7,
        C::Gravel => 8,
        C::Light => 9,
        C::Green => 10,
    }
}

/// A roof vertex's alpha: the building's night occupancy and condition as
/// [`pack_facade`] packs them, and in the code's top bits the face's covering
/// ([`roof_covering`]) — where a wall's vertices carry their eaves and
/// material, which a roof has no use for. Kept apart from the walls' code, so
/// the shader reading a covering off a roof never reads a wall's material.
#[must_use]
pub fn pack_roof(f: Facade, covering: u8) -> f32 {
    let base = pack_facade(Facade {
        occupancy: f.occupancy,
        condition: f.condition,
        ..Default::default()
    });
    base + 4.0 * (u32::from(covering.min(15)) << 13) as f32
}

/// The shader's code for a measured wall material: 1 brick, 2 render, 3 panel
/// (`building_windows.wesl::wall_material`).
#[must_use]
pub fn wall_material(m: cartopolis_geo::formats::facades::Material) -> u8 {
    use cartopolis_geo::formats::facades::Material as M;
    match m {
        M::Brick => 1,
        M::Render => 2,
        M::Panel => 3,
    }
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
    let code = u32::from(f.shop)
        | (u32::from(f.era.min(7)) << 1)
        | (eave << 4)
        | (u32::from(f.material.min(3)) << 13);
    occupancy + 4.0 * code as f32
}

/// The shader's decode, mirrored for the tests that pin the two together.
#[cfg(test)]
fn unpack_facade(a: f32) -> Facade {
    let code = ((a + 1.0 / 32.0) / 4.0).floor();
    let raw = (a - 4.0 * code).max(0.0);
    let code = code as u32;
    let eave = (code >> 4) & 511;
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
        material: ((code >> 13) & 3) as u8,
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
    /// Metres the window lattice is slid along the face, so its columns
    /// stand where the measured ones do ([`WallFace::measure`]).
    shift: f32,
    /// The measured lattice's pane and storey, packed for the shader's UV1;
    /// `[0, 0]` where none was measured.
    windows: [f32; 2],
}

impl WallFace {
    /// Lay the face's windows on a lattice measured from the city model
    /// (`formats::facades::Windows`): `a` and `b` its ends in this face's
    /// frame (world XZ), the rest as measured. `false` — and nothing changed —
    /// when the measured wall is not this face: turned more than 15° from it,
    /// more than 1.5 m off its plane, or beside it rather than on it.
    pub fn measure(
        &mut self,
        a: Vec2,
        b: Vec2,
        w: &cartopolis_geo::formats::facades::Windows,
    ) -> bool {
        let t = self.tangent();
        let dir = (b - a).normalize_or_zero();
        if dir.dot(t).abs() < 15f32.to_radians().cos() {
            return false;
        }
        let mid = (a + b) * 0.5;
        if (self.outward.dot(mid) - self.d).abs() > 1.5 {
            return false;
        }
        let (ua, ub) = (t.dot(a) - self.u0, t.dot(b) - self.u0);
        if ua.max(ub) < 0.0 || ua.min(ub) > self.len {
            return false;
        }
        if !(0.6..=5.1).contains(&w.bay) {
            return false;
        }
        // The first column's centre in this face's own along-coordinate: `x0`
        // from `a`, whichever way along the face `a` lies.
        let first = if ub >= ua { ua + w.x0 } else { ua - w.x0 };
        self.bay = w.bay;
        // The shader centres a window at (k + ½)·bay; slide so one is at `first`.
        self.shift = (first - 0.5 * w.bay).rem_euclid(w.bay);
        let cm = |v: f32, max: u32| ((v * 100.0).round().max(0.0) as u32).min(max);
        self.windows = [
            (cm(w.w, 511) + 512 * cm(w.h, 511)) as f32,
            (cm(w.st, 511) + 512 * cm(w.z0, 2047) + 1_048_576 * u32::from(w.n.min(15))) as f32,
        ];
        true
    }

    /// The UV1 its vertices carry: its measured window lattice, packed as
    /// `building_windows.wesl` reads it — `[w_cm + 512·h_cm, storey_cm +
    /// 512·z0_cm + 2²⁰·rows]` — or `[0, 0]`. Under 2²⁴, so exact in the
    /// float it travels as.
    #[must_use]
    pub fn windows(&self) -> [f32; 2] {
        self.windows
    }
    #[must_use]
    pub fn tangent(&self) -> Vec2 {
        Vec2::new(-self.outward.y, self.outward.x)
    }

    /// The UV0 its vertices carry: `x` metres along the face from its start
    /// (less a measured lattice's [`shift`](Self::measure)), `y` the packed
    /// layout (see [`WallFaces`]).
    #[must_use]
    pub fn uv(&self, p: Vec2) -> [f32; 2] {
        let bay_cm = if self.len < MIN_WINDOWED_M {
            1
        } else {
            ((self.bay * 100.0).round() as u32).clamp(2, 511)
        };
        // In the same slid coordinate as the windows, so the shader keeps
        // them clear of it; a door the slide puts before the start keeps none.
        let door = self
            .door
            .map(|u| u - self.shift)
            .filter(|&u| u > 0.0)
            .map_or(0, |u| ((u / DOOR_STEP_M).round() as u32).clamp(1, 2047));
        [
            self.tangent().dot(p) - self.u0 - self.shift,
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
                    shift: 0.0,
                    windows: [0.0, 0.0],
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

    /// The box's long wall on z = 0, measured with windows 2.4 m apart whose
    /// first column stands 1.0 m from one end: its lattice moves there, and
    /// the wall beside it, turned 90°, is not that wall.
    #[test]
    fn a_measured_lattice_lands_on_its_own_face() {
        let (pts, tris) = box_building();
        let mut walls = WallFaces::new(&pts, &tris, 1);
        let w = cartopolis_geo::formats::facades::Windows {
            a: (0.0, 0.0),
            b: (0.0, 0.0),
            bay: 2.4,
            h: 1.6,
            n: 3,
            st: 3.1,
            w: 1.1,
            x0: 1.0,
            z0: 4.2,
        };
        let (a, b) = (Vec2::new(0.0, 0.2), Vec2::new(10.0, 0.2));
        let i = walls
            .faces
            .iter()
            .position(|f| f.len == 10.0 && f.d.abs() < 0.01)
            .unwrap();
        let side = walls.faces.iter().position(|f| f.len == 6.0).unwrap();
        assert!(!walls.faces[side].clone().measure(a, b, &w));
        let face = &mut walls.faces[i];
        assert!(face.measure(a, b, &w));
        // A window's centre, (k + ½)·bay in the slid coordinate, lands on the
        // first column wherever along the face `a` lies.
        let at_first = face.tangent().dot(a + (b - a).normalize() * 1.0);
        let along = at_first - face.u0 - face.shift;
        let k = along / face.bay - 0.5;
        assert!(
            (k - k.round()).abs() < 1e-3,
            "{along} on a {} m bay",
            face.bay
        );
        // The shader's decode of the packed lattice.
        let [x, y] = face.windows();
        let (x, y) = (x as u32, y as u32);
        assert_eq!(
            (x % 512, x / 512, y % 512, (y / 512) % 2048, y / 1_048_576),
            (110, 160, 310, 420, 3)
        );
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
                    // The largest code there is: every bit of it set.
                    material: 3,
                };
                let a = super::pack_facade(f);
                for wobble in [-0.02, 0.0, 0.02] {
                    let back = super::unpack_facade(a + wobble);
                    assert_eq!(back.condition, condition, "{f:?} at {wobble}");
                    assert_eq!((back.shop, back.era), (true, 4), "{f:?} at {wobble}");
                    assert_eq!(back.material, 3, "{f:?} at {wobble}");
                    assert!((back.eave_m.unwrap() - 102.2).abs() <= 0.1, "{back:?}");
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
    fn a_roof_carries_its_covering_and_never_the_walls_material() {
        let f = super::Facade {
            occupancy: 1.25,
            shop: true,
            era: 4,
            eave_m: Some(102.2),
            condition: super::Condition::Kept,
            material: 3,
        };
        for covering in 0..=10u8 {
            let a = super::pack_roof(f, covering);
            for wobble in [-0.02, 0.0, 0.02] {
                let code = ((a + wobble + 1.0 / 32.0) / 4.0).floor() as u32;
                assert_eq!(
                    (code >> 13) & 15,
                    u32::from(covering),
                    "{covering} at {wobble}"
                );
                let back = super::unpack_facade(a + wobble);
                assert!((back.occupancy - 1.25).abs() < 0.05, "{back:?}");
                assert_eq!(
                    back.material & 1,
                    covering & 1,
                    "bits 13–14 are the covering's"
                );
            }
        }
        // Unmeasured is exactly the facade's own occupancy and condition.
        assert_eq!(super::unpack_facade(super::pack_roof(f, 0)).eave_m, None);
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
                material: 1,
            },
            super::Facade {
                occupancy: 0.25,
                shop: false,
                era: 4,
                eave_m: Some(102.2),
                condition: super::Condition::Kept,
                material: 0,
            },
            super::Facade {
                occupancy: 0.5,
                shop: false,
                era: 2,
                eave_m: Some(12.0),
                condition: super::Condition::Kept,
                material: 2,
            },
        ] {
            let back = super::unpack_facade(super::pack_facade(f));
            assert!(
                (back.occupancy - f.occupancy).abs() <= 1.0 / 32.0,
                "{back:?}"
            );
            assert_eq!((back.shop, back.era), (f.shop, f.era));
            assert_eq!(back.material, f.material);
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
