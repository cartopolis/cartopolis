//! A building's design: its storeys, walls, doors, rooms and stairs, as one small
//! document per building (`docs/notes/building-designs.md`).
//!
//! This is the shape everything inside a building is described in, whichever way
//! it was made:
//!
//! * **generated** — [`generate`] lays one out from what the registers know: the
//!   BAG outline, its entrances, and the shell's measured height. That is every
//!   building nobody has designed, and it is exactly the interior the client used
//!   to build by hand in `systems::interiors` (an inset room per storey and one
//!   straight flight against the longest wall);
//! * **drawn** in the atlas web UI's floor-plan editor, which starts from the
//!   generated one;
//! * **imported** from an IFC model, later.
//!
//! and [`BuildingDesign::compile`] is the one place a design becomes something to
//! draw (a mesh), something to stand on (floor levels and stair ramps) and
//! something to walk into (wall segments). One compiler for all three sources is
//! the point: a drawn building and a generated one cannot drift into behaving
//! differently, because there is only one thing that decides how a wall behaves.
//!
//! # Why the structure follows IFC
//!
//! Storey → room, and walls carrying their openings, is IFC's spatial tree cut
//! down to what a visitor walks through. Keeping that skeleton (and an `id` on
//! every part) is what lets a model be re-imported as an update rather than a
//! replacement and a drawn design be exported back out. IFC's *geometry* —
//! extrusions and boolean cuts — is not copied: a wall here is a line, a
//! thickness and a height, which is what an editor can draw and a phone can mesh.
//!
//! # The frame
//!
//! Metres, **x east and y north**, from [`BuildingDesign::origin`] (lat, lng). Up
//! is the third coordinate of every compiled position. The client's world frame
//! runs +z *south*, so it mirrors y on the way in — and with it every triangle's
//! winding; that is the client's job, and doing it there keeps this module's
//! arithmetic in the frame a person drawing a floor plan thinks in.

use serde::{Deserialize, Serialize};

/// The format revision. Bump it when a reader would misread an older document.
pub const FORMAT_VERSION: u32 = 1;

/// The zoom the designs are filed under, one file per cell — the same cells
/// every other building tree uses.
pub const DESIGN_ZOOM: u32 = 14;

/// Floor-to-floor height of a generated storey, metres.
pub const STOREY_M: f32 = 3.2;

/// How far a storey's ceiling sits below the floor above it: the slab and the
/// joists. A storey's walls stop here.
pub const SLAB_M: f32 = 0.3;

/// A floor's height above its storey's elevation. Clear of the whole coplanar
/// ground stack, which lives inside a centimetre of zero.
pub const FLOOR_Y: f32 = 0.06;

/// Most storeys a *generated* design stacks.
pub const MAX_GENERATED_STOREYS: usize = 8;

/// Horizontal length of a generated flight, metres. 43° over a 3.2 m storey:
/// steep, as a Dutch staircase is.
pub const STAIR_RUN_M: f32 = 3.4;

/// Width of a generated flight, metres.
pub const STAIR_WIDTH_M: f32 = 0.95;

/// Wall left clear at each end of a generated flight, metres.
const STAIR_END_CLEAR_M: f32 = 0.4;

/// Treads per flight when one is drawn.
const STAIR_STEPS: usize = 16;

/// How far a generated design's outer walls stand inside the register's
/// outline — roughly a real wall's thickness, and what keeps them out of the
/// shell's own plane.
pub const WALL_INSET_M: f32 = 0.22;

/// Smallest room worth building, square metres.
const MIN_ROOM_M2: f32 = 4.0;

/// The doorway a generated design cuts at each entrance: the width the collider
/// opens there (`collision::DOOR_HALF_WIDTH` either side), so the hole in the wall
/// and the hole in the collision are the same hole.
pub const GENERATED_DOOR_WIDTH_M: f32 = 2.2;

/// A generated doorway's height, metres.
pub const DOOR_HEIGHT_M: f32 = 2.3;

/// How far from a wall an entrance point may lie and still be that wall's door.
const DOOR_SNAP_M: f32 = 2.2;

/// An opening whose sill is above this is a window, whatever it says it is: it
/// cannot be walked through, so it does not open the collider.
const WALKABLE_SILL_M: f32 = 0.3;

/// Ceilings on what a document may hold. A design is something a person draws or
/// an importer writes, and it is published to every client that walks past the
/// building, so it is bounded where it is parsed rather than trusted.
pub const MAX_STOREYS: usize = 40;
pub const MAX_WALLS: usize = 4000;
pub const MAX_OPENINGS_PER_WALL: usize = 32;
pub const MAX_POLYGON_POINTS: usize = 512;
pub const MAX_EXTENT_M: f32 = 500.0;
pub const MAX_NAME_LEN: usize = 120;

/// Linear RGB for the surfaces, the colours the generated rooms always had.
pub const WALL_COLOR: [f32; 4] = [0.62, 0.60, 0.57, 1.0];
pub const FLOOR_COLOR: [f32; 4] = [0.40, 0.33, 0.27, 1.0];
pub const CEILING_COLOR: [f32; 4] = [0.72, 0.71, 0.69, 1.0];

type P2 = [f32; 2];

/// One building's design. See the module header.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BuildingDesign {
    pub version: u32,
    /// The BAG identificatie of the building this is the design of.
    pub pand: String,
    /// Bumped by atlas on every publish. A reader that holds an older revision
    /// holds an older design.
    #[serde(default)]
    pub revision: u64,
    /// Where the frame's origin is, `[lat, lng]`.
    pub origin: [f64; 2],
    #[serde(default)]
    pub source: DesignSource,
    /// Ground floor first.
    pub storeys: Vec<Storey>,
}

/// Where a design came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesignSource {
    /// Laid out from the registers by [`generate`]; nobody has touched it.
    #[default]
    Generated,
    /// Drawn or edited in the floor-plan editor.
    Editor,
    /// Converted from an IFC model.
    Ifc,
}

/// One storey. Its walls stand on its floor and stop [`SLAB_M`] under the next.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Storey {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Height of this storey's floor above the ground, metres.
    pub elevation: f32,
    /// Floor to floor, metres.
    pub height: f32,
    /// The floor slab's outline. Empty is no floor — a void, or a storey that is
    /// all stairwell.
    #[serde(default)]
    pub floor: Vec<P2>,
    #[serde(default)]
    pub walls: Vec<Wall>,
    #[serde(default)]
    pub rooms: Vec<Room>,
    /// Flights rising out of this storey to the next one.
    #[serde(default)]
    pub stairs: Vec<Stair>,
}

/// A wall: a line, a thickness, and the openings in it. It is as tall as its
/// storey's ceiling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Wall {
    #[serde(default)]
    pub id: String,
    pub a: P2,
    pub b: P2,
    #[serde(default = "default_thickness")]
    pub thickness: f32,
    /// One of the building's outer walls. Those stand just inside the shell and
    /// show only their inner face; the shell is what is seen from the street,
    /// and the shell's collider is what stops the avatar at them.
    #[serde(default)]
    pub exterior: bool,
    #[serde(default)]
    pub openings: Vec<Opening>,
}

fn default_thickness() -> f32 {
    0.1
}

/// A door or a window in a wall.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Opening {
    #[serde(default)]
    pub id: String,
    pub kind: OpeningKind,
    /// Distance of the opening's centre from the wall's `a` end, metres.
    pub at: f32,
    pub width: f32,
    pub height: f32,
    /// Height of the bottom of the opening above the floor. 0 for a door.
    #[serde(default)]
    pub sill: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpeningKind {
    Door,
    Window,
}

/// A named room. Its outline is a label and a tint, not a wall: the walls are
/// what stand.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Room {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub outline: Vec<P2>,
}

/// One straight flight rising out of its storey to the next.
///
/// It occupies the rectangle `foot + dir·[0, run] + left·[0, width]`, where
/// `left` is `dir` turned a quarter turn anticlockwise. That rectangle is also the
/// hole cut in the floor above, and the area the ramp the avatar walks up spans.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stair {
    #[serde(default)]
    pub id: String,
    pub foot: P2,
    /// Direction of ascent. Normalised on use.
    pub dir: P2,
    pub width: f32,
    pub run: f32,
}

impl Stair {
    fn frame(&self) -> Option<(P2, P2)> {
        let d = norm(self.dir)?;
        Some((d, [-d[1], d[0]]))
    }

    /// The four corners, anticlockwise from the foot.
    #[must_use]
    pub fn rect(&self) -> Option<[P2; 4]> {
        let (d, l) = self.frame()?;
        let r = scale(d, self.run);
        let w = scale(l, self.width);
        Some([
            self.foot,
            add(self.foot, r),
            add(add(self.foot, r), w),
            add(self.foot, w),
        ])
    }

    /// Where `p` lies on the flight as a fraction of its run, or `None` off it.
    #[must_use]
    pub fn fraction_at(&self, p: P2) -> Option<f32> {
        let (d, l) = self.frame()?;
        let rel = sub(p, self.foot);
        let along = dot(rel, d);
        let across = dot(rel, l);
        (along >= 0.0 && along <= self.run && across >= 0.0 && across <= self.width)
            .then(|| along / self.run)
    }
}

// ─── Validation ─────────────────────────────────────────────────────────────

impl BuildingDesign {
    /// Parse and check a document. The error is a sentence for the person who
    /// drew it.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let design: Self = serde_json::from_str(text).map_err(|e| format!("not a design: {e}"))?;
        design.validate()?;
        Ok(design)
    }

    /// Whether this document is one a client can be handed. Everything a parser
    /// accepts but a mesher should not have to defend against is refused here.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != FORMAT_VERSION {
            return Err(format!(
                "this is a version {} design and only version {FORMAT_VERSION} is understood",
                self.version
            ));
        }
        if self.pand.is_empty() || self.pand.len() > 40 {
            return Err("the design does not say which building it is for".into());
        }
        if !(self.origin[0].is_finite()
            && self.origin[1].is_finite()
            && self.origin[0].abs() <= 90.0
            && self.origin[1].abs() <= 180.0)
        {
            return Err("the design's origin is not a place on Earth".into());
        }
        if self.storeys.is_empty() {
            return Err("a design needs at least one storey".into());
        }
        if self.storeys.len() > MAX_STOREYS {
            return Err(format!("more than {MAX_STOREYS} storeys"));
        }
        let walls: usize = self.storeys.iter().map(|s| s.walls.len()).sum();
        if walls > MAX_WALLS {
            return Err(format!("{walls} walls, and the limit is {MAX_WALLS}"));
        }
        let mut below: Option<&Storey> = None;
        for (k, s) in self.storeys.iter().enumerate() {
            let label = if s.name.is_empty() {
                format!("storey {k}")
            } else {
                s.name.clone()
            };
            if s.name.len() > MAX_NAME_LEN || s.id.len() > MAX_NAME_LEN {
                return Err(format!("{label}: name too long"));
            }
            if !s.elevation.is_finite() || s.elevation.abs() > 200.0 {
                return Err(format!("{label}: elevation out of range"));
            }
            if !(s.height.is_finite() && (2.0..=12.0).contains(&s.height)) {
                return Err(format!("{label}: a storey is 2 to 12 m tall"));
            }
            if let Some(b) = below {
                if s.elevation < b.elevation + b.height - 0.01 {
                    return Err(format!("{label} starts inside the storey below it"));
                }
            }
            check_polygon(&s.floor, true).map_err(|e| format!("{label} floor: {e}"))?;
            for w in &s.walls {
                check_point(w.a).map_err(|e| format!("{label} wall: {e}"))?;
                check_point(w.b).map_err(|e| format!("{label} wall: {e}"))?;
                let len = dist(w.a, w.b);
                if len < 0.05 {
                    return Err(format!("{label}: a wall is shorter than 5 cm"));
                }
                if !(w.thickness.is_finite() && (0.0..=1.5).contains(&w.thickness)) {
                    return Err(format!("{label}: a wall is 0 to 1.5 m thick"));
                }
                if w.openings.len() > MAX_OPENINGS_PER_WALL {
                    return Err(format!("{label}: too many openings in one wall"));
                }
                for o in &w.openings {
                    let ok = o.at.is_finite()
                        && o.width.is_finite()
                        && o.height.is_finite()
                        && o.sill.is_finite()
                        && o.width >= 0.2
                        && o.height >= 0.2
                        && o.sill >= 0.0
                        && o.at - o.width * 0.5 >= -0.01
                        && o.at + o.width * 0.5 <= len + 0.01;
                    if !ok {
                        return Err(format!(
                            "{label}: an opening does not fit in its {len:.2} m wall"
                        ));
                    }
                }
            }
            for r in &s.rooms {
                if r.name.len() > MAX_NAME_LEN {
                    return Err(format!("{label}: a room name is too long"));
                }
                check_polygon(&r.outline, false).map_err(|e| format!("{label} room: {e}"))?;
            }
            for st in &s.stairs {
                check_point(st.foot).map_err(|e| format!("{label} stair: {e}"))?;
                if norm(st.dir).is_none() {
                    return Err(format!("{label}: a stair has no direction"));
                }
                if !(st.width.is_finite()
                    && st.run.is_finite()
                    && (0.5..=5.0).contains(&st.width)
                    && (1.0..=15.0).contains(&st.run))
                {
                    return Err(format!("{label}: a stair is 0.5–5 m wide and 1–15 m long"));
                }
                if k + 1 == self.storeys.len() {
                    return Err(format!("{label}: a stair on the top storey leads nowhere"));
                }
            }
            below = Some(s);
        }
        Ok(())
    }
}

fn check_point(p: P2) -> Result<(), String> {
    if p[0].is_finite()
        && p[1].is_finite()
        && p[0].abs() <= MAX_EXTENT_M
        && p[1].abs() <= MAX_EXTENT_M
    {
        Ok(())
    } else {
        Err(format!(
            "a point lies more than {MAX_EXTENT_M} m from the origin"
        ))
    }
}

fn check_polygon(ring: &[P2], may_be_empty: bool) -> Result<(), String> {
    if ring.is_empty() && may_be_empty {
        return Ok(());
    }
    if ring.len() < 3 {
        return Err("an outline needs at least three points".into());
    }
    if ring.len() > MAX_POLYGON_POINTS {
        return Err(format!(
            "an outline has more than {MAX_POLYGON_POINTS} points"
        ));
    }
    for &p in ring {
        check_point(p)?;
    }
    Ok(())
}

// ─── The frame ──────────────────────────────────────────────────────────────

/// Metres per degree of latitude — the building-scale tangent plane, good to
/// millimetres over one structure.
pub const M_PER_DEG_LAT: f64 = 111_320.0;

/// A lat/lng as metres east/north of `origin`.
#[must_use]
pub fn to_local(origin: [f64; 2], lat: f64, lng: f64) -> P2 {
    let k = origin[0].to_radians().cos();
    [
        ((lng - origin[1]) * M_PER_DEG_LAT * k) as f32,
        ((lat - origin[0]) * M_PER_DEG_LAT) as f32,
    ]
}

/// The inverse of [`to_local`].
#[must_use]
pub fn to_lat_lng(origin: [f64; 2], p: P2) -> (f64, f64) {
    let k = origin[0].to_radians().cos();
    (
        origin[0] + f64::from(p[1]) / M_PER_DEG_LAT,
        origin[1] + f64::from(p[0]) / (M_PER_DEG_LAT * k),
    )
}

// ─── Generating a design from the registers ─────────────────────────────────

/// How many storeys fit in a shell whose eaves datum is `height_m` — floored,
/// because the whole stack has to fit *under* the roof. `None` (no shell yet) is
/// one storey.
#[must_use]
pub fn storeys_for(height_m: Option<f32>) -> usize {
    let Some(h) = height_m else {
        return 1;
    };
    ((h / STOREY_M).floor() as usize).clamp(1, MAX_GENERATED_STOREYS)
}

/// The shell's eaves datum: the 70th percentile of its roof vertices' heights
/// (3DBAG's own `b3_h_dak_70p`). The ridge would invent a floor in the rafters of
/// every pitched Dutch roof. Falls back to every vertex for a cell that tags no
/// roof triangles.
#[must_use]
pub fn shell_datum_height(heights: &[f32], roof: &[f32]) -> Option<f32> {
    let mut hs: Vec<f32> = if roof.is_empty() {
        heights.to_vec()
    } else {
        roof.to_vec()
    };
    hs.retain(|h| h.is_finite());
    if hs.is_empty() {
        return None;
    }
    hs.sort_by(f32::total_cmp);
    let at = ((hs.len() - 1) as f32 * 0.7).round() as usize;
    Some(hs[at].max(0.0))
}

/// The ring a generated design's outer walls stand on, or `None` when the outline
/// is too small to hold a room at all.
#[must_use]
pub fn room_ring(outline: &[P2]) -> Option<Vec<P2>> {
    let inset = inset_ring(outline, WALL_INSET_M);
    (inset.len() >= 3 && ring_area(&inset) >= MIN_ROOM_M2).then_some(inset)
}

/// Lay out a building nobody has designed: an inset room per storey, the
/// entrances cut on the ground floor, and one straight flight against the longest
/// wall reaching every storey above. A building with nowhere to put the flight
/// gets its ground floor only — a floor nobody can reach is not a floor.
///
/// `outline` and `doors` are in this design's frame (see [`to_local`]).
#[must_use]
pub fn generate(
    pand: &str,
    origin: [f64; 2],
    outline: &[P2],
    height_m: Option<f32>,
    doors: &[P2],
) -> Option<BuildingDesign> {
    let ring = room_ring(outline)?;
    let wanted = storeys_for(height_m);
    let stair = (wanted > 1).then(|| find_stair(&ring)).flatten();
    let storeys_n = if stair.is_some() { wanted } else { 1 };
    let mut storeys = Vec::with_capacity(storeys_n);
    for k in 0..storeys_n {
        let n = ring.len();
        let mut walls = Vec::with_capacity(n);
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let len = dist(a, b);
            if len < 0.05 {
                continue;
            }
            let mut openings = Vec::new();
            if k == 0 {
                for (j, &d) in doors.iter().enumerate() {
                    let dir = scale(sub(b, a), 1.0 / len);
                    let t = dot(sub(d, a), dir);
                    let near = add(a, scale(dir, t.clamp(0.0, len)));
                    if dist(near, d) > DOOR_SNAP_M {
                        continue;
                    }
                    // Clamped so the opening fits the wall: a door a metre from a
                    // corner still opens, it just opens up to the corner.
                    let width = GENERATED_DOOR_WIDTH_M.min(len);
                    let at = t.clamp(width * 0.5, len - width * 0.5);
                    openings.push(Opening {
                        id: format!("d{j}"),
                        kind: OpeningKind::Door,
                        at,
                        width,
                        height: DOOR_HEIGHT_M,
                        sill: 0.0,
                    });
                }
            }
            walls.push(Wall {
                id: format!("s{k}w{i}"),
                a,
                b,
                thickness: 0.0,
                exterior: true,
                openings,
            });
        }
        storeys.push(Storey {
            id: format!("s{k}"),
            name: storey_name(k),
            elevation: k as f32 * STOREY_M,
            height: STOREY_M,
            floor: ring.clone(),
            walls,
            rooms: Vec::new(),
            stairs: if k + 1 < storeys_n {
                stair.clone().into_iter().collect()
            } else {
                Vec::new()
            },
        });
    }
    Some(BuildingDesign {
        version: FORMAT_VERSION,
        pand: pand.to_string(),
        revision: 0,
        origin,
        source: DesignSource::Generated,
        storeys,
    })
}

/// The name a person would give storey `k`.
#[must_use]
pub fn storey_name(k: usize) -> String {
    match k {
        0 => "Ground floor".to_string(),
        1 => "1st floor".to_string(),
        2 => "2nd floor".to_string(),
        3 => "3rd floor".to_string(),
        n => format!("{n}th floor"),
    }
}

/// A straight flight against the longest wall that can take one, centred on it,
/// rising into the room. Centred rather than chosen so the answer is the same
/// every time it is asked — a staircase that moved would be a poltergeist.
fn find_stair(ring: &[P2]) -> Option<Stair> {
    let n = ring.len();
    let mut best: Option<(f32, Stair)> = None;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let len = dist(a, b);
        if len < STAIR_RUN_M + 2.0 * STAIR_END_CLEAR_M {
            continue;
        }
        let mut dir = scale(sub(b, a), 1.0 / len);
        let mut foot = add(a, scale(dir, (len - STAIR_RUN_M) * 0.5));
        // The flight spans to the left of its direction of ascent; turn it round
        // where the room is on the right, so it stands in the room.
        let left = [-dir[1], dir[0]];
        let mid = scale(add(a, b), 0.5);
        if !point_in_ring(ring, add(mid, scale(left, 0.05))) {
            foot = add(foot, scale(dir, STAIR_RUN_M));
            dir = scale(dir, -1.0);
        }
        let stair = Stair {
            id: "stair".to_string(),
            foot,
            dir,
            width: STAIR_WIDTH_M,
            run: STAIR_RUN_M,
        };
        // Deep enough here, not merely long enough along the wall: a corridor
        // passes the length test and has no room for a flight. Two corners lie
        // exactly on the wall, where an even-odd test answers by rounding, so
        // each is nudged toward the middle of the flight — into the room if the
        // flight is in it — before it is asked. Nudging along the wall instead
        // left them on the edge, and whether a real terrace house got its
        // stairs depended on where the frame's origin was.
        let Some(rect) = stair.rect() else { continue };
        let centre = scale(add(rect[0], rect[2]), 0.5);
        if !rect
            .iter()
            .all(|&c| point_in_ring(ring, shrink_toward(c, centre, 0.02)))
        {
            continue;
        }
        if best.as_ref().is_none_or(|(l, _)| len > *l) {
            best = Some((len, stair));
        }
    }
    best.map(|(_, s)| s)
}

/// `p` moved `by` metres toward `to` — so a corner lying exactly on a wall is
/// tested as inside it.
fn shrink_toward(p: P2, to: P2, by: f32) -> P2 {
    match norm(sub(to, p)) {
        Some(d) => add(p, scale(d, by)),
        None => p,
    }
}

// ─── Compiling ──────────────────────────────────────────────────────────────

/// A design made into what the client draws and stands on. All in the design's
/// frame: `[east, up, north]` for 3D, `[east, north]` for 2D.
#[derive(Clone, Debug, Default)]
pub struct Compiled {
    pub mesh: MeshData,
    /// Every walkable floor: a level and the polygons (outer ring, then holes) it
    /// covers.
    pub floors: Vec<FloorLevel>,
    /// Every flight, as the slope the avatar walks up.
    pub ramps: Vec<Ramp>,
    /// The inner walls the avatar is stopped by, already split around their
    /// doorways. Outer walls are not here: the shell's collider is their collider.
    pub walls: Vec<WallSegment>,
    /// The ground floor's outer doorways, centre points — where the shell's
    /// collider has to open.
    pub entrances: Vec<P2>,
}

#[derive(Clone, Debug, Default)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct FloorLevel {
    /// The surface's height above the ground.
    pub y: f32,
    pub polygons: Vec<Vec<Vec<P2>>>,
}

impl FloorLevel {
    /// Whether `p` is on this floor (inside an outer ring, outside its holes).
    #[must_use]
    pub fn covers(&self, p: P2) -> bool {
        self.polygons.iter().any(|poly| {
            poly.first().is_some_and(|outer| point_in_ring(outer, p))
                && !poly[1..].iter().any(|h| point_in_ring(h, p))
        })
    }
}

#[derive(Clone, Debug)]
pub struct Ramp {
    pub stair: Stair,
    /// The surface at the foot.
    pub y0: f32,
    pub rise: f32,
}

impl Ramp {
    /// The ramp's surface over `p`, or `None` off it.
    #[must_use]
    pub fn height_at(&self, p: P2) -> Option<f32> {
        self.stair.fraction_at(p).map(|t| self.y0 + t * self.rise)
    }
}

/// A piece of wall that stops the avatar, between two heights above the ground.
#[derive(Clone, Copy, Debug)]
pub struct WallSegment {
    pub a: P2,
    pub b: P2,
    pub bottom: f32,
    pub top: f32,
}

impl BuildingDesign {
    /// Make the design into geometry. See [`Compiled`].
    ///
    /// Assumes [`Self::validate`] passed; a degenerate part is skipped rather than
    /// panicked over, because a document reaching a client has been validated by
    /// atlas and a generated one is built from what this module itself checks.
    #[must_use]
    pub fn compile(&self) -> Compiled {
        self.compile_cut(None)
    }

    /// [`Self::compile`], with the **mesh** cut away above storey `cut`: the
    /// storeys over it are not drawn and neither is its own ceiling, so the
    /// storey can be looked down into. The floors, ramps and walls are the whole
    /// building whatever the cut — what you can see is a choice, what you can
    /// stand on is not, and an avatar upstairs must not fall because somebody
    /// is looking at the ground floor.
    #[must_use]
    pub fn compile_cut(&self, cut: Option<usize>) -> Compiled {
        let mut out = Compiled::default();
        let mut hidden = MeshData::default();
        let n = self.storeys.len();
        for (k, s) in self.storeys.iter().enumerate() {
            let drawn = cut.is_none_or(|c| k <= c);
            let mesh: &mut MeshData = if drawn { &mut out.mesh } else { &mut hidden };
            let floor_y = s.elevation + FLOOR_Y;
            let ceiling_y = s.elevation + s.height - SLAB_M;
            let wall_h = (ceiling_y - floor_y).max(0.5);

            // The floor, less the wells of the flights arriving from below.
            let wells_in: Vec<[P2; 4]> = if k > 0 {
                self.storeys[k - 1]
                    .stairs
                    .iter()
                    .filter_map(Stair::rect)
                    .collect()
            } else {
                Vec::new()
            };
            let wells_out: Vec<[P2; 4]> = if k + 1 < n {
                s.stairs.iter().filter_map(Stair::rect).collect()
            } else {
                Vec::new()
            };
            if s.floor.len() >= 3 {
                let floor = subtract(&s.floor, &wells_in);
                for poly in &floor {
                    mesh.slab(poly, floor_y, FLOOR_COLOR, true);
                }
                out.floors.push(FloorLevel {
                    y: floor_y,
                    polygons: floor,
                });
                // The ceiling has the outgoing flights' wells in it: a flight
                // capped by the ceiling it rises through is a flight into a lid.
                if cut != Some(k) {
                    for poly in &subtract(&s.floor, &wells_out) {
                        mesh.slab(poly, ceiling_y, CEILING_COLOR, false);
                    }
                }
            }

            for w in &s.walls {
                let len = dist(w.a, w.b);
                let Some(dir) = norm(sub(w.b, w.a)) else {
                    continue;
                };
                let left = [-dir[1], dir[0]];
                // Pieces along the wall: full-height solid runs, and around each
                // opening the bits below its sill and above its head.
                let mut cuts: Vec<(f32, f32, f32, f32)> = w
                    .openings
                    .iter()
                    .map(|o| {
                        let lo = (o.at - o.width * 0.5).clamp(0.0, len);
                        let hi = (o.at + o.width * 0.5).clamp(0.0, len);
                        let bottom = o.sill.clamp(0.0, wall_h);
                        let top = (o.sill + o.height).clamp(bottom, wall_h);
                        (lo, hi, bottom, top)
                    })
                    .filter(|c| c.1 > c.0)
                    .collect();
                cuts.sort_by(|x, y| x.0.total_cmp(&y.0));
                let mut pieces: Vec<(f32, f32, f32, f32)> = Vec::new();
                let mut at = 0.0f32;
                for &(lo, hi, bottom, top) in &cuts {
                    if lo > at {
                        pieces.push((at, lo, 0.0, wall_h));
                    }
                    let (lo, hi) = (lo.max(at), hi.max(at));
                    if hi > lo {
                        if bottom > 0.0 {
                            pieces.push((lo, hi, 0.0, bottom));
                        }
                        if top < wall_h {
                            pieces.push((lo, hi, top, wall_h));
                        }
                    }
                    at = at.max(hi);
                }
                if at < len {
                    pieces.push((at, len, 0.0, wall_h));
                }

                if w.exterior {
                    // One face, toward the inside — which side that is, asked of
                    // the floor rather than derived from a winding nobody promised.
                    let inward = if s.floor.len() >= 3 {
                        let mid = scale(add(w.a, w.b), 0.5);
                        if point_in_ring(&s.floor, add(mid, scale(left, 0.05))) {
                            left
                        } else {
                            scale(left, -1.0)
                        }
                    } else {
                        left
                    };
                    for &(t0, t1, b, t) in &pieces {
                        let (p, q) = (add(w.a, scale(dir, t0)), add(w.a, scale(dir, t1)));
                        mesh.face(p, q, inward, floor_y + b, floor_y + t, WALL_COLOR);
                    }
                    if k == 0 {
                        for o in w
                            .openings
                            .iter()
                            .filter(|o| o.kind == OpeningKind::Door && o.sill < WALKABLE_SILL_M)
                        {
                            out.entrances.push(add(w.a, scale(dir, o.at)));
                        }
                    }
                } else {
                    // Both faces, half the thickness either side of the line, and
                    // the two ends capped.
                    let half = (w.thickness * 0.5).max(0.01);
                    let off = scale(left, half);
                    for &(t0, t1, b, t) in &pieces {
                        let (p, q) = (add(w.a, scale(dir, t0)), add(w.a, scale(dir, t1)));
                        let (y0, y1) = (floor_y + b, floor_y + t);
                        mesh.face(add(p, off), add(q, off), left, y0, y1, WALL_COLOR);
                        mesh.face(
                            sub(p, off),
                            sub(q, off),
                            scale(left, -1.0),
                            y0,
                            y1,
                            WALL_COLOR,
                        );
                        // The reveals: the wall's cut ends, at the wall's ends and
                        // either side of each opening.
                        mesh.face(
                            sub(p, off),
                            add(p, off),
                            scale(dir, -1.0),
                            y0,
                            y1,
                            WALL_COLOR,
                        );
                        mesh.face(sub(q, off), add(q, off), dir, y0, y1, WALL_COLOR);
                        if b > 0.0 {
                            // The underside of a lintel.
                            mesh.flat(
                                [add(p, off), add(q, off), sub(q, off), sub(p, off)],
                                y0,
                                false,
                                WALL_COLOR,
                            );
                        }
                        if t < wall_h {
                            // The top of a sill.
                            mesh.flat(
                                [add(p, off), add(q, off), sub(q, off), sub(p, off)],
                                y1,
                                true,
                                WALL_COLOR,
                            );
                        }
                    }
                    // The collider: every piece that reaches the floor stops the
                    // avatar; a gap under a lintel is a doorway.
                    for &(t0, t1, b, _) in &pieces {
                        if b < WALKABLE_SILL_M {
                            out.walls.push(WallSegment {
                                a: add(w.a, scale(dir, t0)),
                                b: add(w.a, scale(dir, t1)),
                                bottom: floor_y,
                                top: ceiling_y,
                            });
                        }
                    }
                }
            }

            if k + 1 < n {
                let rise = self.storeys[k + 1].elevation - s.elevation;
                for st in &s.stairs {
                    if st.rect().is_none() || rise <= 0.0 {
                        continue;
                    }
                    mesh.flight(st, floor_y, rise);
                    out.ramps.push(Ramp {
                        stair: st.clone(),
                        y0: floor_y,
                        rise,
                    });
                }
            }
        }
        out
    }

    /// Space windows along every outer wall, a bay about every 2.4 m, clear of
    /// the doors — the starting facade an owner's first save is made from.
    ///
    /// Not part of [`generate`], on purpose: a building nobody has designed keeps
    /// its shell's painted windows, and a hole in a room's wall where the street
    /// shows no window would be a view out through the brick. Once a design is
    /// saved, its outer walls *are* the facade (`lod22_facade`), and a design with
    /// no windows would turn a building into a blank brick box.
    pub fn with_facade_windows(&mut self) {
        const BAY_M: f32 = 2.4;
        for s in &mut self.storeys {
            for w in s.walls.iter_mut().filter(|w| w.exterior) {
                let len = dist(w.a, w.b);
                if len < 1.5 {
                    continue;
                }
                let count = ((len / BAY_M).round() as usize).max(1);
                let width = (len / count as f32 * 0.6).min(1.2);
                if width < 0.4 {
                    continue;
                }
                let doors: Vec<(f32, f32)> = w
                    .openings
                    .iter()
                    .filter(|o| o.kind == OpeningKind::Door)
                    .map(|o| (o.at, o.width))
                    .collect();
                for i in 0..count {
                    let at = len * (i as f32 + 0.5) / count as f32;
                    if doors
                        .iter()
                        .any(|&(d, dw)| (d - at).abs() < (dw + width) * 0.5 + 0.15)
                    {
                        continue;
                    }
                    w.openings.push(Opening {
                        id: format!("{}n{i}", w.id),
                        kind: OpeningKind::Window,
                        at,
                        width,
                        height: 1.3,
                        sill: 0.9,
                    });
                }
            }
        }
    }

    /// The ground floor's outer doorways, without compiling anything else.
    #[must_use]
    pub fn entrances(&self) -> Vec<P2> {
        let Some(s) = self.storeys.first() else {
            return Vec::new();
        };
        s.walls
            .iter()
            .filter(|w| w.exterior)
            .flat_map(|w| {
                let dir = norm(sub(w.b, w.a)).unwrap_or([1.0, 0.0]);
                w.openings
                    .iter()
                    .filter(|o| o.kind == OpeningKind::Door && o.sill < WALKABLE_SILL_M)
                    .map(move |o| add(w.a, scale(dir, o.at)))
            })
            .collect()
    }
}

/// `ring` less the rectangles, as polygons with holes. Plain when there is
/// nothing to cut, so the common case does not round-trip through the overlay.
fn subtract(ring: &[P2], cuts: &[[P2; 4]]) -> Vec<Vec<Vec<P2>>> {
    use i_overlay::core::fill_rule::FillRule;
    use i_overlay::core::overlay::{Overlay, ShapeType};
    use i_overlay::core::overlay_rule::OverlayRule;
    use i_overlay::i_float::int::point::IntPoint;

    if cuts.is_empty() {
        return vec![vec![ring.to_vec()]];
    }
    // Millimetres on an integer lattice, which is the interface `road_stitch`
    // uses for the same reason: exact, with nothing to tune.
    let mm = |p: P2| {
        IntPoint::new(
            (p[0] * 1000.0).round() as i32,
            (p[1] * 1000.0).round() as i32,
        )
    };
    let wound = |pts: &[P2]| -> Vec<IntPoint<i32>> {
        let mut out: Vec<IntPoint<i32>> = pts.iter().map(|&p| mm(p)).collect();
        if signed_area(pts) < 0.0 {
            out.reverse();
        }
        out
    };
    let mut overlay: Overlay<i32> = Overlay::new(16);
    overlay.add_contour(&wound(ring), ShapeType::Subject);
    for c in cuts {
        overlay.add_contour(&wound(c), ShapeType::Clip);
    }
    overlay
        .overlay(OverlayRule::Difference, FillRule::NonZero)
        .into_iter()
        .map(|shape| {
            shape
                .into_iter()
                .map(|c| {
                    c.into_iter()
                        .map(|p| [p.x as f32 / 1000.0, p.y as f32 / 1000.0])
                        .collect::<Vec<P2>>()
                })
                .filter(|c| c.len() >= 3)
                .collect::<Vec<_>>()
        })
        .filter(|poly: &Vec<Vec<P2>>| !poly.is_empty())
        .collect()
}

impl MeshData {
    fn push(&mut self, p: [f32; 3], n: [f32; 3], c: [f32; 4]) -> u32 {
        let i = self.positions.len() as u32;
        self.positions.push(p);
        self.normals.push(n);
        self.colors.push(c);
        i
    }

    /// Three indices, wound so the triangle faces `n`.
    fn tri(&mut self, i: [u32; 3], n: [f32; 3]) {
        let [a, b, c] = i.map(|k| self.positions[k as usize]);
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cross = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        // Right-handed with `[east, up, north]` mapped to (x, y, z) — the client
        // mirrors north into its +z-south frame and reverses every triangle with it.
        let facing = cross[0] * n[0] + cross[1] * n[1] + cross[2] * n[2];
        if facing >= 0.0 {
            self.indices.extend(i);
        } else {
            self.indices.extend([i[0], i[2], i[1]]);
        }
    }

    /// A vertical panel from `p` to `q` between two heights, facing `normal`.
    fn face(&mut self, p: P2, q: P2, normal: P2, bottom: f32, top: f32, c: [f32; 4]) {
        if top - bottom <= 1e-4 || dist(p, q) <= 1e-4 {
            return;
        }
        let n = [normal[0], 0.0, normal[1]];
        let v = [
            self.push([p[0], bottom, p[1]], n, c),
            self.push([q[0], bottom, q[1]], n, c),
            self.push([q[0], top, q[1]], n, c),
            self.push([p[0], top, p[1]], n, c),
        ];
        self.tri([v[0], v[1], v[2]], n);
        self.tri([v[0], v[2], v[3]], n);
    }

    /// A horizontal quad.
    fn flat(&mut self, corners: [P2; 4], y: f32, up: bool, c: [f32; 4]) {
        let n = if up {
            [0.0, 1.0, 0.0]
        } else {
            [0.0, -1.0, 0.0]
        };
        let v = corners.map(|p| self.push([p[0], y, p[1]], n, c));
        self.tri([v[0], v[1], v[2]], n);
        self.tri([v[0], v[2], v[3]], n);
    }

    /// A slab: one polygon with its holes, earcut. Each triangle is wound to its
    /// normal rather than trusting the rings' winding.
    fn slab(&mut self, poly: &[Vec<P2>], y: f32, c: [f32; 4], up: bool) {
        let mut flat: Vec<f64> = Vec::new();
        let mut holes: Vec<usize> = Vec::new();
        let mut pts: Vec<P2> = Vec::new();
        for (i, ring) in poly.iter().enumerate() {
            if ring.len() < 3 {
                continue;
            }
            if i > 0 {
                holes.push(pts.len());
            }
            for &p in ring {
                flat.extend([f64::from(p[0]), f64::from(p[1])]);
                pts.push(p);
            }
        }
        if pts.len() < 3 {
            return;
        }
        let Ok(tris) = earcutr::earcut(&flat, &holes, 2) else {
            return;
        };
        let n = if up {
            [0.0, 1.0, 0.0]
        } else {
            [0.0, -1.0, 0.0]
        };
        let base = self.positions.len() as u32;
        for &p in &pts {
            self.push([p[0], y, p[1]], n, c);
        }
        for t in tris.chunks_exact(3) {
            self.tri(
                [base + t[0] as u32, base + t[1] as u32, base + t[2] as u32],
                n,
            );
        }
    }

    /// One flight: treads, risers, the sloped underside and the open side.
    ///
    /// The underside and the side are not decoration. Nothing here is
    /// double-sided, so bare treads are invisible from below and the storey above
    /// shows straight through them — and standing under the stairs is somewhere
    /// the avatar ends up.
    fn flight(&mut self, st: &Stair, bottom: f32, rise_total: f32) {
        let Some((d, l)) = st.frame() else { return };
        let going = st.run / STAIR_STEPS as f32;
        let rise = rise_total / STAIR_STEPS as f32;
        let across = scale(l, st.width);
        let back_n = [-d[0], -d[1]];
        for s in 0..STAIR_STEPS {
            let y = bottom + (s + 1) as f32 * rise;
            let front = add(st.foot, scale(d, s as f32 * going));
            let back = add(front, scale(d, going));
            self.flat(
                [front, back, add(back, across), add(front, across)],
                y,
                true,
                WALL_COLOR,
            );
            self.face(front, add(front, across), back_n, y - rise, y, FLOOR_COLOR);
        }
        // The underside: one sloped quad, its normal perpendicular to the ramp and
        // pointing away from the treads.
        let top = add(st.foot, scale(d, st.run));
        let len = (st.run * st.run + rise_total * rise_total).sqrt();
        let (sx, sy) = (st.run / len, rise_total / len);
        let down = [d[0] * sy, -sx, d[1] * sy];
        let v = [
            self.push([st.foot[0], bottom, st.foot[1]], down, FLOOR_COLOR),
            self.push(
                [st.foot[0] + across[0], bottom, st.foot[1] + across[1]],
                down,
                FLOOR_COLOR,
            ),
            self.push(
                [top[0] + across[0], bottom + rise_total, top[1] + across[1]],
                down,
                FLOOR_COLOR,
            ),
            self.push([top[0], bottom + rise_total, top[1]], down, FLOOR_COLOR),
        ];
        self.tri([v[0], v[1], v[2]], down);
        self.tri([v[0], v[2], v[3]], down);
        // The open side, as the triangle the flight's profile is. The other side
        // stands against a wall, or is the far side of a two-sided flight nobody
        // asked for.
        let side_n = [l[0], 0.0, l[1]];
        let foot_side = add(st.foot, across);
        let top_side = add(top, across);
        let t = [
            self.push([foot_side[0], bottom, foot_side[1]], side_n, FLOOR_COLOR),
            self.push([top_side[0], bottom, top_side[1]], side_n, FLOOR_COLOR),
            self.push(
                [top_side[0], bottom + rise_total, top_side[1]],
                side_n,
                FLOOR_COLOR,
            ),
        ];
        self.tri(t, side_n);
    }
}

// ─── 2D helpers ─────────────────────────────────────────────────────────────

fn add(a: P2, b: P2) -> P2 {
    [a[0] + b[0], a[1] + b[1]]
}
fn sub(a: P2, b: P2) -> P2 {
    [a[0] - b[0], a[1] - b[1]]
}
fn scale(a: P2, k: f32) -> P2 {
    [a[0] * k, a[1] * k]
}
fn dot(a: P2, b: P2) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}
fn dist(a: P2, b: P2) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
fn norm(a: P2) -> Option<P2> {
    let l = a[0].hypot(a[1]);
    (l.is_finite() && l > 1e-6).then(|| scale(a, 1.0 / l))
}

/// Even-odd ray cast against a bare ring.
#[must_use]
pub fn point_in_ring(ring: &[P2], p: P2) -> bool {
    let mut inside = false;
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (u, v) = (ring[i], ring[j]);
        if (u[1] > p[1]) != (v[1] > p[1]) {
            let x = u[0] + (p[1] - u[1]) / (v[1] - u[1]) * (v[0] - u[0]);
            if p[0] < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

fn signed_area(ring: &[P2]) -> f32 {
    let mut a = 0.0;
    for i in 0..ring.len() {
        let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
        a += p[0] * q[1] - q[0] * p[1];
    }
    a * 0.5
}

fn ring_area(ring: &[P2]) -> f32 {
    signed_area(ring).abs()
}

/// Move every edge of a ring inward by `by` metres.
///
/// Edge offset with a corner join, not a vertex push: pushing each vertex along
/// its own bisector collapses a thin building into a bow tie, and these are Dutch
/// canal houses. Where the offset edges no longer meet, the corner is dropped,
/// which turns a sliver into a shorter wall rather than into a knot.
#[must_use]
pub fn inset_ring(ring: &[P2], by: f32) -> Vec<P2> {
    let n = ring.len();
    if n < 3 {
        return Vec::new();
    }
    let sign = if signed_area(ring) > 0.0 { 1.0 } else { -1.0 };
    let edge_normal = |a: P2, b: P2| -> P2 {
        let d = sub(b, a);
        let len = d[0].hypot(d[1]);
        if len <= f32::EPSILON {
            [0.0, 0.0]
        } else {
            [-d[1] / len, d[0] / len]
        }
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = ring[(i + n - 1) % n];
        let cur = ring[i];
        let next = ring[(i + 1) % n];
        let n0 = scale(edge_normal(prev, cur), sign * by);
        let n1 = scale(edge_normal(cur, next), sign * by);
        let bis = add(n0, n1);
        let Some(half) = norm(bis) else { continue };
        let Some(n0u) = norm(n0) else { continue };
        let cos = dot(half, n0u);
        if cos.abs() < 0.2 {
            continue;
        }
        out.push(add(cur, scale(half, by / cos)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 6 × 12 m terrace house, door in the middle of the south wall, with a
    /// shell tall enough for three storeys.
    fn house() -> BuildingDesign {
        let outline = [[0.0, 0.0], [6.0, 0.0], [6.0, 12.0], [0.0, 12.0]];
        generate(
            "0014100010938796",
            [53.2, 6.56],
            &outline,
            Some(9.8),
            &[[3.0, 0.0]],
        )
        .expect("a house this size holds a room")
    }

    #[test]
    fn a_generated_house_has_its_storeys_its_stair_and_its_door() {
        let d = house();
        assert_eq!(d.storeys.len(), 3);
        assert!(d.validate().is_ok(), "{:?}", d.validate());
        // One flight out of every storey but the top.
        assert_eq!(d.storeys[0].stairs.len(), 1);
        assert_eq!(d.storeys[1].stairs.len(), 1);
        assert!(d.storeys[2].stairs.is_empty());
        // The door is on the ground floor only, on the south wall.
        let doors: usize = d.storeys[0].walls.iter().map(|w| w.openings.len()).sum();
        assert_eq!(doors, 1);
        assert_eq!(d.entrances().len(), 1);
        assert!((d.entrances()[0][0] - 3.0).abs() < 0.3);
        assert!(d.storeys[1].walls.iter().all(|w| w.openings.is_empty()));
    }

    #[test]
    fn the_flight_stands_in_the_room_whichever_way_the_outline_winds() {
        for outline in [
            vec![[0.0, 0.0], [6.0, 0.0], [6.0, 12.0], [0.0, 12.0]],
            vec![[0.0, 12.0], [6.0, 12.0], [6.0, 0.0], [0.0, 0.0]],
        ] {
            let d = generate("x", [53.2, 6.56], &outline, Some(7.0), &[]).unwrap();
            let st = &d.storeys[0].stairs[0];
            let ring = &d.storeys[0].floor;
            for c in st.rect().unwrap() {
                assert!(
                    point_in_ring(ring, shrink_toward(c, [3.0, 6.0], 0.02)),
                    "a corner of the flight is outside the room: {c:?}"
                );
            }
        }
    }

    /// Prinsenstraat 24–26, Groningen (pand 0014100010938796): a real BAG
    /// outline, with a nearly collinear fifth corner, under an 11 m shell. It
    /// came out as one storey — the flight's corners on the wall were tested
    /// for being inside the room while lying exactly on its edge.
    #[test]
    fn a_real_terrace_house_gets_its_stairs_and_its_storeys() {
        let fp = [
            (53.215_249_7, 6.570_901_7),
            (53.215_303_6, 6.570_820_0),
            (53.215_214_4, 6.570_614_4),
            (53.215_158_9, 6.570_699_4),
            (53.215_229_8, 6.570_856_8),
        ];
        let n = fp.len() as f64;
        let centre = [
            fp.iter().map(|p| p.0).sum::<f64>() / n,
            fp.iter().map(|p| p.1).sum::<f64>() / n,
        ];
        // Atlas puts the origin at the centre, the client at the first corner.
        for origin in [[fp[0].0, fp[0].1], centre] {
            let outline: Vec<[f32; 2]> = fp.iter().map(|&(a, b)| to_local(origin, a, b)).collect();
            for ring in [outline.clone(), outline.iter().rev().copied().collect()] {
                let d = generate("0014100010938796", origin, &ring, Some(11.15), &[]).unwrap();
                assert_eq!(d.storeys.len(), 3, "an 11 m shell holds three storeys");
                assert!(d.validate().is_ok());
            }
        }
    }

    /// The editor's starting facade: windows on every outer wall of every
    /// storey, none over a door, and still a valid document.
    #[test]
    fn facade_windows_keep_clear_of_the_door() {
        let mut d = house();
        d.with_facade_windows();
        assert!(d.validate().is_ok(), "{:?}", d.validate());
        let ground = &d.storeys[0];
        let south = &ground.walls[0];
        let door = south
            .openings
            .iter()
            .find(|o| o.kind == OpeningKind::Door)
            .unwrap();
        for w in south
            .openings
            .iter()
            .filter(|o| o.kind == OpeningKind::Window)
        {
            assert!((w.at - door.at).abs() >= (w.width + door.width) * 0.5);
        }
        let windows: usize = d
            .storeys
            .iter()
            .flat_map(|s| &s.walls)
            .map(|w| {
                w.openings
                    .iter()
                    .filter(|o| o.kind == OpeningKind::Window)
                    .count()
            })
            .sum();
        assert!(windows >= 12, "{windows}");
    }

    #[test]
    fn a_building_too_small_for_a_flight_gets_one_storey() {
        let outline = [[0.0, 0.0], [3.0, 0.0], [3.0, 3.0], [0.0, 3.0]];
        let d = generate("x", [53.2, 6.56], &outline, Some(9.0), &[]).unwrap();
        assert_eq!(d.storeys.len(), 1);
    }

    #[test]
    fn the_floor_above_has_the_well_cut_out_and_the_ramp_climbs_into_it() {
        let d = house();
        let c = d.compile();
        assert_eq!(c.floors.len(), 3);
        assert_eq!(c.ramps.len(), 2);
        let ramp = &c.ramps[0];
        let mid = {
            let r = ramp.stair.rect().unwrap();
            [(r[0][0] + r[2][0]) * 0.5, (r[0][1] + r[2][1]) * 0.5]
        };
        // Over the well, the first floor is not a floor…
        assert!(c.floors[0].covers(mid));
        assert!(!c.floors[1].covers(mid), "the well is filled in");
        // …and the ramp is, half way up.
        let h = ramp.height_at(mid).unwrap();
        assert!((h - (FLOOR_Y + STOREY_M * 0.5)).abs() < 0.05, "{h}");
        assert!(!c.mesh.indices.is_empty());
    }

    /// Cut at the first floor, nothing above it is drawn and neither is its
    /// ceiling — but every floor is still there to stand on.
    #[test]
    fn a_cut_hides_what_is_above_and_keeps_what_is_underfoot() {
        let d = house();
        let whole = d.compile();
        let cut = d.compile_cut(Some(1));
        let top = |c: &Compiled| {
            c.mesh
                .positions
                .iter()
                .map(|p| p[1])
                .fold(f32::MIN, f32::max)
        };
        // Nothing is higher than the top of the flight rising out of the first
        // floor — kept on purpose, it is the way up — which lands on the second
        // floor's level.
        assert!(
            top(&cut) <= STOREY_M * 2.0 + FLOOR_Y + 1e-3,
            "{}",
            top(&cut)
        );
        assert!(top(&whole) > STOREY_M * 3.0 - SLAB_M - 1e-3);
        assert!(cut.mesh.indices.len() < whole.mesh.indices.len());
        // No down-facing surface at the first floor's ceiling height.
        let ceiling = STOREY_M * 2.0 - SLAB_M;
        assert!(!cut
            .mesh
            .positions
            .iter()
            .zip(&cut.mesh.normals)
            .any(|(p, n)| n[1] < -0.9 && (p[1] - ceiling).abs() < 1e-3));
        assert_eq!(cut.floors.len(), whole.floors.len());
        assert_eq!(cut.ramps.len(), whole.ramps.len());
    }

    #[test]
    fn an_inner_wall_stops_the_avatar_except_at_its_door() {
        let mut d = house();
        d.storeys[0].walls.push(Wall {
            id: "p".into(),
            a: [0.22, 6.0],
            b: [5.78, 6.0],
            thickness: 0.1,
            exterior: false,
            openings: vec![Opening {
                id: "pd".into(),
                kind: OpeningKind::Door,
                at: 2.0,
                width: 0.9,
                height: 2.1,
                sill: 0.0,
            }],
        });
        assert!(d.validate().is_ok());
        let c = d.compile();
        let segs: Vec<&WallSegment> = c.walls.iter().collect();
        assert_eq!(segs.len(), 2, "one each side of the door");
        let gap_lo = segs[0].b[0];
        let gap_hi = segs[1].a[0];
        assert!((gap_hi - gap_lo - 0.9).abs() < 1e-3);
    }

    #[test]
    fn a_window_does_not_open_the_wall() {
        let mut d = house();
        d.storeys[0].walls.push(Wall {
            id: "p".into(),
            a: [0.22, 6.0],
            b: [5.78, 6.0],
            thickness: 0.1,
            exterior: false,
            openings: vec![Opening {
                id: "pw".into(),
                kind: OpeningKind::Window,
                at: 2.0,
                width: 0.9,
                height: 1.0,
                sill: 0.9,
            }],
        });
        let c = d.compile();
        let covered: f32 = c.walls.iter().map(|s| dist(s.a, s.b)).sum();
        assert!((covered - 5.56).abs() < 1e-3, "{covered}");
    }

    #[test]
    fn nonsense_is_refused_with_a_reason() {
        let mut d = house();
        d.storeys[1].elevation = 1.0;
        assert!(d
            .validate()
            .unwrap_err()
            .contains("inside the storey below"));
        let mut d = house();
        d.storeys[0].walls[0].openings.push(Opening {
            id: String::new(),
            kind: OpeningKind::Door,
            at: 100.0,
            width: 1.0,
            height: 2.0,
            sill: 0.0,
        });
        assert!(d.validate().unwrap_err().contains("does not fit"));
        let mut d = house();
        d.storeys[2].stairs = d.storeys[0].stairs.clone();
        assert!(d.validate().unwrap_err().contains("leads nowhere"));
    }

    #[test]
    fn a_design_survives_the_round_trip_through_json() {
        let d = house();
        let text = serde_json::to_string(&d).unwrap();
        assert_eq!(BuildingDesign::from_json(&text).unwrap(), d);
    }

    #[test]
    fn local_metres_round_trip() {
        let o = [53.2194, 6.5665];
        let p = to_local(o, 53.2200, 6.5700);
        let (lat, lng) = to_lat_lng(o, p);
        assert!((lat - 53.22).abs() < 1e-7 && (lng - 6.57).abs() < 1e-7);
        assert!(p[0] > 0.0 && p[1] > 0.0, "east and north are positive");
    }

    #[test]
    fn the_datum_is_the_eaves_not_the_ridge() {
        // Eight eaves vertices at 6 m and two ridge vertices at 10 m.
        let roof = [6.0, 6.0, 6.0, 6.0, 6.0, 6.0, 6.0, 6.0, 10.0, 10.0];
        assert_eq!(shell_datum_height(&[], &roof), Some(6.0));
        assert_eq!(storeys_for(Some(6.0)), 1);
        assert_eq!(storeys_for(Some(6.5)), 2);
    }
}
