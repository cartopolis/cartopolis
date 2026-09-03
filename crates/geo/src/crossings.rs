//! The level crossings ProRail registers, raised into the world as the
//! installation standing on each one: masts, lights and the booms that drop
//! across the road.
//!
//! No Bevy: register points in, [`SurfaceMesh`]es out. The fetching half is
//! `systems::crossings`, and `docs/notes/prorail-level-crossings.md` has the
//! endpoint, the licence, the census and the three traps in the data.
//!
//! # What this adds
//!
//! A railway reaches this client as a Shortbread `streets` feature with
//! `kind = rail`, drawn by [`crate::road_texture`] as a 3.2 m ballast ribbon with
//! the rails painted into the texture. Nothing three-dimensional stands on a
//! railway anywhere: [`FURNITURE_TABLE`](crate::furniture) names no `railway`
//! key at all, and the BGT supplies only the paved *surface* under a crossing. So
//! a level crossing has been a street ribbon meeting a rail ribbon with no
//! equipment on it, which is the one place on a railway where a person on the
//! ground would notice the absence.
//!
//! # One of the register's seven collections
//!
//! `overweg` is the only one of the ProRail service's seven collections that
//! changes the picture. Track axes and the corridor centreline would lay a second
//! set of rail ribbons over the ones already drawn — the coplanar z-fight this
//! codebase keeps paying for — and switches and diamond crossings have nothing to
//! be drawn *on*, the rails being a painted texture rather than modelled steel.
//! The note records all six rejections so the check is not repeated.
//!
//! # Drop, never substitute
//!
//! [`CROSSING_TABLE`] follows the rule
//! [`furniture`](crate::furniture)'s does: **a code the table does not name draws
//! nothing.** `Geen` (2,035 of 3,917 — a crossing with no installation at all)
//! and `Onbekend` are named by their absence for the same reason, and six further
//! codes covering 42 features are left out because their expansion is not
//! established and a guessed form is an invented survey.
//!
//! Orientation is the same rule in the other direction. A crossing whose road
//! this tile does not carry is **not drawn**, rather than drawn at a guessed
//! angle: the register's point sits on the rail centreline, so the one thing that
//! is certain about a hashed yaw here is that some of them lay the boom along the
//! track.
//!
//! # What is deliberately not here
//!
//! * **The St Andrew's cross.** Every Dutch crossing has one and the register
//!   does not record it — `aantal_andreas` is null on all 3,917 features — so
//!   drawing it would be plausible rather than measured.
//! * **Animation.** A boom that lowers needs a train, and there is no train. The
//!   booms stand raised, which is what a crossing looks like almost all the time.
//! * **A collider on the bar.** The masts *are* solid — [`mast_centres`] hands
//!   their positions to the client, which makes one circle per mast — but the
//!   raised bar is not covered by it: it hangs 0.12 m further inboard than the
//!   circle reaches and starts a metre off the ground, and props are height-free
//!   circles. Crossings are still not
//!   [`FurnitureInstance`](crate::furniture::FurnitureInstance)s; they publish
//!   bare colliders instead, because a furniture form would need a mesh recipe
//!   nothing uses.
//!
//! That is where this layer parts from [`nav_marks`](crate::nav_marks), which it
//! otherwise follows. A buoy gets no collider because it stands on water the
//! avatar cannot walk on, so a circle there is an invisible wall in the middle of
//! a canal. A crossing mast stands on a verge half a metre outside the kerb of a
//! street the avatar walks along — the lamp-column case, not the buoy case.

use crate::furniture::{paint_multiplier, MeshBuf, Part, Tone, WayField, WayFilter, BOX_BYTES};
use crate::vector_tiles::{palette, SurfaceGroup, SurfaceMesh, TileData};

/// Hard ceiling on crossings per streamed cell, on the same terms as
/// [`MAX_MARKS_PER_CELL`](crate::nav_marks::MAX_MARKS_PER_CELL) and
/// [`MAX_FURNITURE_PER_CELL`](crate::furniture::MAX_FURNITURE_PER_CELL): one
/// command buffer must not be handed an unbounded pile of triangles. The whole
/// country holds 3,917 crossings and the densest z14 cell measured is Utrecht
/// Centraal with 8, so this is headroom rather than a working limit, and
/// truncation is logged rather than silent.
pub const MAX_CROSSINGS_PER_CELL: usize = 200;

/// How far a crossing looks for the road it belongs to.
///
/// Wider than [`furniture`](crate::furniture)'s 20 m, and for a different reason:
/// a bench is *set back* from its path by a few metres, while a crossing point is
/// on the track the road crosses and should be within a metre of the road's own
/// line. The margin is for the collection's own caveat — its description says the
/// point is "a point on the rail centreline **in the neighbourhood of** a
/// crossing", not the middle of one.
const ORIENT_RADIUS_M: f32 = 30.0;

/// Centre-to-centre spacing of adjacent tracks, metres. Dutch double track is
/// laid at about this.
const TRACK_SPACING_M: f32 = 4.5;

/// How far beyond the outermost rail the installation stands, metres.
const SETBACK_M: f32 = 3.5;

/// Upper bound on `aantal_sporen` before it is treated as nonsense. The register
/// reaches 14, which is a real station throat; anything past this would spread one
/// crossing's masts across a neighbourhood.
const MAX_TRACKS: u32 = 16;

/// Vertical faces per crossing, for the byte accounting. Two installations, each
/// a mast, a lamp head and up to [`BOOM_BANDS`] boom bands.
const MAX_BOXES_PER_CROSSING: usize = 2 * (2 + BOOM_BANDS);

/// What one crossing costs to upload at most — the flush unit
/// [`build_crossing_meshes`] reserves before it adds one.
const CROSSING_MAX_BYTES: usize = MAX_BOXES_PER_CROSSING * BOX_BYTES;

/// Boxes a boom is banded into. Three is the fewest that reads as banded rather
/// than as two halves.
const BOOM_BANDS: usize = 3;

/// Height of the boom above the road, metres — where a raised boom's pivot sits.
const BOOM_Y: f32 = 1.05;

/// How far outside the kerb a mast stands, metres. Just clear of the
/// carriageway, so a boom sweeps over the road rather than a mast standing in
/// it.
const VERGE_M: f32 = 0.5;

const MAST_HALF_W: f32 = 0.11;
/// Half the width of the bar's painted face — 0.26 m across. Named for the face
/// rather than for a height or a width because the bar stands *raised*: the face
/// lies across the road on local x, and the bar's length runs up local y. A
/// constant named for one attitude is how a reader concludes the bar is in the
/// other one.
const BOOM_HALF_FACE: f32 = 0.13;
/// Half the bar's thickness — 0.16 m through, on the along-road axis in either
/// attitude.
const BOOM_HALF_T: f32 = 0.08;

/// The two colours a boom is banded in, and the colour of a lamp.
///
/// This layer bends the furniture palette on the terms
/// [`nav_marks`](crate::nav_marks) states for the same trade: the red-and-white is
/// the datum — it is what makes a pole beside a railway read as a level crossing
/// rather than as a lamppost — so it is a muted signal colour rather than a
/// furniture grey. Still well back from the real paint, which is fluorescent.
const BOOM_RED: u32 = 0xB2_43_3C;
const BOOM_WHITE: u32 = 0xE9_E5_DB;
const LAMP_RED: u32 = 0xC0_4A_3E;

// ─── The vocabulary ──────────────────────────────────────────────────────────

/// The form of installation standing on a crossing.
///
/// Four forms for eighteen registered codes, because the codes distinguish who
/// operates the thing (`H…` for hand-worked) and this distinguishes what is
/// standing there — an installation worked by hand looks the same as an automatic
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossingKind {
    /// Half barriers: a boom over the entry half of each carriageway, so the two
    /// masts stand diagonally opposite. The commonest form in the country.
    HalfBarrier,
    /// The footpath and cycleway variant of the same installation — shorter
    /// booms, lower masts, narrower road.
    MiniBarrier,
    /// Booms across the full width of the road.
    FullBarrier,
    /// Lights and no boom at all: a mast with a lamp head on it.
    Lights,
}

/// `overweg_type` → form. First hit wins, and **a code that is not here draws
/// nothing** — the rule [`FURNITURE_TABLE`](crate::furniture) states, for the
/// same reason: a wrongly-formed installation asserts a survey nobody made.
///
/// The counts are the national census of 2026-08-30 (3,917 features), and what is
/// *not* here matters as much as what is:
///
/// * `Geen` (2,035) is a crossing with **no installation** — a farm track over a
///   branch line with nothing but a sign. Drawing anything there is the single
///   biggest way this layer could lie.
/// * `Onbekend` (40) is the register saying it does not know.
/// * `WILO` (16), `VKL` (9), `HAVIO` (9), `EBO` (4), `ADOB` (1) — 39 features
///   whose expansion is not established. `VKL` in particular looks like road
///   traffic signals rather than a crossing installation. Naming them would be
///   guessing at a form, which is what this table exists to avoid; the note
///   records them so the next reader knows they were considered.
pub const CROSSING_TABLE: &[(&str, CrossingKind)] = &[
    // Automatische Halve Overwegbomen — half barriers, 1,157.
    ("AHOB", CrossingKind::HalfBarrier),
    // The same installation worked by hand, 5.
    ("HAHOB", CrossingKind::HalfBarrier),
    // The footpath/cycleway variant, 381 — every sampled one is an
    // `Openbare overweg (voetpad)` or a cycle route.
    ("AHOB-MINI", CrossingKind::MiniBarrier),
    // Automatische Overwegbomen — booms over the full width, 137.
    ("AOB", CrossingKind::FullBarrier),
    // The `…LI` (lichtinstallatie) and `…KI` (knipperlichtinstallatie) families:
    // lights, no boom. 63 + 24 + 21 + 8 + 3 + 3 + 1.
    ("ALI", CrossingKind::Lights),
    ("ALI-B", CrossingKind::Lights),
    ("AKI", CrossingKind::Lights),
    ("HAKI", CrossingKind::Lights),
    ("HALI", CrossingKind::Lights),
    ("HALI-B", CrossingKind::Lights),
    ("HBKI", CrossingKind::Lights),
];

/// Classify one `overweg_type`. `None` for everything the table does not name.
///
/// Case- and space-insensitive: the register writes these in upper case
/// throughout, but a code that arrives as `ahob` should not silently empty a
/// city's worth of crossings.
#[must_use]
pub fn crossing_kind(overweg_type: &str) -> Option<CrossingKind> {
    let want = overweg_type.trim().to_ascii_uppercase();
    CROSSING_TABLE
        .iter()
        .find(|(code, _)| *code == want)
        .map(|(_, kind)| *kind)
}

impl CrossingKind {
    /// Mast height, and the half-carriageway to size the installation for when
    /// the road's own width is not known — both metres.
    ///
    /// Measured rather than stylised, on the terms
    /// [`furniture::parts`](crate::furniture::parts) states: a boom really does
    /// sit about a metre off the road, and a crossing built at a doll's-house
    /// scale is worse than none.
    ///
    /// The **height** belongs to the form and stays here: a mini installation on
    /// a wide road is still a mini installation. The **width** does not — it is
    /// the road's, and [`LevelCrossing::road_half`] carries the real one off the
    /// same tile segment the yaw came from. What is left here is the fallback,
    /// which is what these numbers always were: 3.5 m describes a 7 m road, i.e.
    /// a residential street, for every `AHOB`, `HAHOB`, `AOB` and lights
    /// installation in the country.
    const fn spec(self) -> (f32, f32) {
        match self {
            Self::HalfBarrier | Self::FullBarrier | Self::Lights => (3.1, 3.5),
            Self::MiniBarrier => (2.4, 1.6),
        }
    }

    /// How far a boom reaches back across the road from its mast, as a multiple
    /// of the half-carriageway. `None` for a form with no boom.
    ///
    /// A multiple rather than a length, because the number that matters is where
    /// the **tip** lands: a half barrier bars the entry half, so its tip meets
    /// the centreline (`1.0` × half, plus the [`VERGE_M`] the mast stands back
    /// by); a full barrier bars the whole width, so its tip reaches the far kerb
    /// (`2.0` ×). At the fallback half-width that is 4.0 m and 2.1 m unchanged,
    /// and 7.5 m for the full barrier against a tabulated 7.6 — the 10 cm is the
    /// accepted cost of deriving the number instead of writing it down.
    const fn boom_span(self) -> Option<f32> {
        match self {
            Self::HalfBarrier | Self::MiniBarrier => Some(1.0),
            Self::FullBarrier => Some(2.0),
            Self::Lights => None,
        }
    }
}

// ─── The placed object ───────────────────────────────────────────────────────

/// A crossing as the register gives it: where it is and what stands on it, but
/// not yet which way round.
///
/// Tile-local metres — origin at the cell's north-west corner, +x east, +z south
/// — which is the frame everything in [`furniture`](crate::furniture) works in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossingPoint {
    pub x: f32,
    pub z: f32,
    pub kind: CrossingKind,
    /// `aantal_sporen`: how many tracks the road crosses. It sets how far apart
    /// the two installations stand, and nothing else.
    pub tracks: u8,
}

/// A crossing with the road's direction resolved: ready to mesh.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelCrossing {
    pub x: f32,
    pub z: f32,
    /// Rotation about +y, radians, laying the object's local **+z along the
    /// road**. See [`place_crossings`] for where it comes from, and
    /// `WayField::direction` for why either of two opposite values is acceptable.
    pub yaw: f32,
    pub kind: CrossingKind,
    pub tracks: u8,
    /// Half the carriageway of the road the yaw came from, metres — the same
    /// tile segment, resolved in one lookup so the two cannot disagree.
    ///
    /// `None` when the field could not answer (the way carries no `kind`, or
    /// its kind is not drawn as a carriageway), and then the form's own fallback
    /// in [`CrossingKind::spec`] is used, which is what every crossing used
    /// before this was carried.
    pub road_half: Option<f32>,
}

/// Turn the cell's register points into oriented crossings, using the tile's own
/// `streets` layer to answer which way the road runs.
///
/// **The rail is excluded from that field**, and it is the one genuinely new
/// piece of logic in this module. `WayField` as the furniture uses it indexes
/// every street, path *and railway* in the tile and reads no `kind` at all; the
/// register puts a crossing's point on the rail centreline by its own
/// description, so the nearest way is the track every single time and every boom
/// would be laid along the rails it is supposed to bar.
///
/// A point with no non-rail way within [`ORIENT_RADIUS_M`] is **dropped**. That
/// is a real loss — a service crossing over a farm track OSM does not carry goes
/// undrawn — and it is the honest one: the alternative is an installation at an
/// angle nothing measured.
///
/// The same lookup answers how *wide* that road is, and it has to be the same
/// one: asking a second time could pick a different way, and a crossing sized
/// for one road and turned to another is worse than either mistake alone. See
/// [`LevelCrossing::road_half`].
#[must_use]
pub fn place_crossings(
    tile: &TileData,
    tile_m: f32,
    points: &[CrossingPoint],
) -> Vec<LevelCrossing> {
    if points.is_empty() {
        return Vec::new();
    }
    let roads = WayField::build_with(tile, tile_m, WayFilter::ExcludingRail);
    place_crossings_against(&roads, points)
}

/// [`place_crossings`] over an already-built road field.
fn place_crossings_against(roads: &WayField, points: &[CrossingPoint]) -> Vec<LevelCrossing> {
    points
        .iter()
        .filter_map(|p| {
            roads
                .direction_and_width(p.x, p.z, ORIENT_RADIUS_M)
                .map(|(yaw, road_half)| LevelCrossing {
                    x: p.x,
                    z: p.z,
                    yaw,
                    kind: p.kind,
                    tracks: p.tracks,
                    road_half,
                })
        })
        .collect()
}

// ─── Meshing ─────────────────────────────────────────────────────────────────

fn tint(rgb: u32) -> [f32; 4] {
    let m = paint_multiplier(rgb);
    [m[0], m[1], m[2], 1.0]
}

/// Mesh a cell's crossings into the furniture group, byte-bounded.
///
/// Same contract as [`build_mark_meshes`](crate::nav_marks::build_mark_meshes)
/// and [`build_line_meshes`](crate::furniture::build_line_meshes): one merged
/// mesh per piece, cut **between** whole crossings so no installation loses a
/// mast, and `max_bytes = None` restores the single-mesh behaviour exactly.
#[must_use]
pub fn build_crossing_meshes(
    crossings: &[LevelCrossing],
    max_bytes: Option<usize>,
) -> Vec<SurfaceMesh> {
    if crossings.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = MeshBuf::default();
    for crossing in crossings {
        // Flush before adding, and only with something to flush — a piece must
        // hold at least one crossing, or a cap under one installation's cost
        // would emit empty meshes forever.
        if let Some(max) = max_bytes {
            if buf.bytes() + CROSSING_MAX_BYTES > max && !buf.positions.is_empty() {
                out.push(std::mem::take(&mut buf).finish(SurfaceGroup::Furniture));
            }
        }
        push_crossing(&mut buf, crossing);
    }
    if !buf.positions.is_empty() {
        out.push(buf.finish(SurfaceGroup::Furniture));
    }
    out
}

/// How far along the road, either side of the register's point, the two
/// installations stand.
fn clearance_m(tracks: u8) -> f32 {
    // `max(1)`: 68 features register zero tracks, which is a crossing over one
    // track that nobody counted rather than a crossing over none.
    let tracks = u32::from(tracks).clamp(1, MAX_TRACKS) as f32;
    (tracks - 1.0) * TRACK_SPACING_M * 0.5 + SETBACK_M
}

/// Where one of a crossing's two installations stands, in the crossing's own
/// object frame: local +x across the road, local +z along it.
#[derive(Clone, Copy)]
struct Installation {
    /// ±1 — the side of the road this mast stands on. The bar hangs inboard of
    /// its mast, so it is the sign that puts it there.
    side: f32,
    /// The mast centre across the road.
    x: f32,
    /// The whole installation's position along the road.
    z: f32,
}

/// Every number one crossing is placed from, derived once.
///
/// It exists so that the boxes [`push_crossing`] meshes and the mast centres
/// [`mast_centres`] hands to the collider index come out of **one** derivation.
/// A second copy of `road_half + VERGE_M` is a collider that sits where the mast
/// used to be after the next edit to either.
struct Layout {
    mast_h: f32,
    /// How far a bar reaches from its pivot, `None` for a form with no boom.
    boom: Option<f32>,
    /// The two approaches, on opposite sides of the road — which is how a half
    /// barrier really stands, each boom covering the half its own approach drives
    /// on. Being diagonal also means the pair reads as one installation from any
    /// angle rather than as two identical posts.
    installations: [Installation; 2],
}

impl Layout {
    fn of(crossing: &LevelCrossing) -> Self {
        let (mast_h, fallback_half) = crossing.kind.spec();
        // The road's own half-width where the tile gave one, the form's fallback
        // where it did not. Everything across the road is derived from this one
        // number, so a mast on the verge and a boom that reaches the centreline
        // cannot come apart.
        let road_half = crossing.road_half.unwrap_or(fallback_half);
        let clearance = clearance_m(crossing.tracks);
        let mast_x = road_half + VERGE_M;
        Self {
            mast_h,
            boom: crossing
                .kind
                .boom_span()
                .map(|span| span * road_half + VERGE_M),
            installations: [
                Installation {
                    side: 1.0,
                    x: mast_x,
                    z: clearance,
                },
                Installation {
                    side: -1.0,
                    x: -mast_x,
                    z: -clearance,
                },
            ],
        }
    }
}

/// Where this crossing's two masts stand, in **tile-local metres** — the frame
/// [`LevelCrossing::x`] and [`LevelCrossing::z`] are in.
///
/// The collider half of the layer: a mast is 0.22 m square and 3.1 m tall, and it
/// stands on a verge beside a street the avatar walks along, so walking through
/// one is exactly the thing `world_places::prop_radius` gives a lamp column its
/// 0.25 m for. The navigation marks keep their zero for the reason a moored boat
/// has one — a mark stands on water nobody can walk on — and that is what
/// separates the two cases.
///
/// **Two centres for every form**, [`CrossingKind::Lights`] included: a lights
/// installation is a mast with a lamp head on it, and only the boom is missing.
/// The numbers are read off the same [`Layout`] `push_crossing` meshes from and
/// rotated by the same [`place_ground`](crate::furniture::place_ground) call
/// `push_box` applies, so the circle cannot drift off the post it stands for.
///
/// Nothing here covers the raised bar: it hangs 0.12 m further inboard and starts
/// [`BOOM_Y`] off the ground, and these colliders are height-free circles.
#[must_use]
pub fn mast_centres(crossing: &LevelCrossing) -> [[f32; 2]; 2] {
    let rot = crossing.yaw.sin_cos();
    Layout::of(crossing)
        .installations
        .map(|inst| crate::furniture::place_ground(crossing.x, crossing.z, rot, inst.x, inst.z))
}

fn push_crossing(buf: &mut MeshBuf, crossing: &LevelCrossing) {
    let layout = Layout::of(crossing);
    let mast_h = layout.mast_h;
    let (sn, cs) = crossing.yaw.sin_cos();

    for inst in layout.installations {
        push(
            buf,
            crossing,
            (sn, cs),
            [inst.x, mast_h * 0.5, inst.z],
            [MAST_HALF_W, mast_h * 0.5, MAST_HALF_W],
            palette::FURNITURE_METAL,
        );
        // The lamp head, facing along the road in both directions — a real one
        // shows to the approach only, but the backboard is what carries the
        // shape at any distance this layer is visible from.
        push(
            buf,
            crossing,
            (sn, cs),
            [inst.x, mast_h + 0.16, inst.z],
            [0.30, 0.16, 0.10],
            LAMP_RED,
        );

        let Some(reach) = layout.boom else {
            continue;
        };
        // The boom stands raised: the bands are stacked in **y** from the pivot
        // at `BOOM_Y` up to `BOOM_Y + reach`, at the mast's own along-road
        // position, so nothing hangs over the carriageway. `push_box` takes a yaw
        // and no tilt, so the 90° turn is expressed by swapping the first two
        // half-extents — the 0.26 m face still lies across the road, the 0.16 m
        // thickness still along it, and the bar's *length* moves to y.
        //
        // Across the road the bar sits one mast-half plus one bar-half inboard of
        // the mast centre, so it stands flush beside the mast rather than inside
        // it.
        let band = reach / BOOM_BANDS as f32;
        let boom_x = inst.x - inst.side * (MAST_HALF_W + BOOM_HALF_FACE);
        for b in 0..BOOM_BANDS {
            let centre = BOOM_Y + band * (b as f32 + 0.5);
            push(
                buf,
                crossing,
                (sn, cs),
                [boom_x, centre, inst.z],
                [BOOM_HALF_FACE, band * 0.5, BOOM_HALF_T],
                if b % 2 == 0 { BOOM_RED } else { BOOM_WHITE },
            );
        }
    }
}

fn push(
    buf: &mut MeshBuf,
    crossing: &LevelCrossing,
    rot: (f32, f32),
    at: [f32; 3],
    half: [f32; 3],
    rgb: u32,
) {
    crate::furniture::push_box(
        buf,
        crossing.x,
        crossing.z,
        rot,
        // `push_box` reads only `at` and `half`; the colour it writes is the
        // `tint` argument. The tone is carried because `Part` is the shape the
        // furniture forms are written in, and it is not read here — this layer
        // paints from a signal palette rather than from the `Tone` table, on the
        // terms `nav_marks` states.
        &Part {
            at,
            half,
            tone: Tone::Metal,
        },
        tint(rgb),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_tiles::{INDEX_BYTES, VERTEX_BYTES};

    fn crossing(kind: CrossingKind, tracks: u8) -> LevelCrossing {
        LevelCrossing {
            x: 40.0,
            z: 40.0,
            yaw: 0.0,
            kind,
            tracks,
            road_half: None,
        }
    }

    /// How far out one installation's mast stands from the crossing point, and
    /// how long its bar is — both metres, **read back off the meshed geometry**,
    /// not recomputed, so this measures what is drawn rather than what
    /// `push_crossing` was asked for.
    ///
    /// At `yaw = 0` the object frame is the world frame, so local +x is across
    /// the road and local +z is along it. The two installations stand at
    /// `±clearance` along, so `z > 0` selects one of them, the `+x` one; within
    /// it the mast is the only box reaching the ground (`y < 0.5`), and its
    /// outer face gives the mast centre.
    ///
    /// The bar stands **raised**, so its length is a height, not a tip: the
    /// bands stack in y from [`BOOM_Y`] up, at a fixed x beside the mast, and
    /// the length is the top of that stack above `BOOM_Y`. The bar's inner face
    /// is the most inboard geometry an installation has — 0.37 m in from the
    /// mast centre, against the lamp head's 0.30 m and the mast's own 0.11 m —
    /// so one x test picks the bands out without having to know which vertices
    /// belong to which box, and answers `None` for a form that has no boom.
    fn across(crossing: &LevelCrossing) -> (f32, Option<f32>) {
        assert_eq!(crossing.yaw, 0.0, "the frame this reads in");
        let meshes = build_crossing_meshes(std::slice::from_ref(crossing), None);
        let mine: Vec<[f32; 3]> = meshes[0]
            .positions
            .iter()
            .filter(|p| p[2] - crossing.z > 0.0)
            .map(|p| [p[0] - crossing.x, p[1], p[2] - crossing.z])
            .collect();
        let mast = mine
            .iter()
            .filter(|p| p[1] < 0.5)
            .map(|p| p[0])
            .fold(f32::MIN, f32::max)
            - MAST_HALF_W;
        // The 1 cm sits well inside the 7 cm between the bar's inner face and
        // the lamp head's, so it separates them without being a knife edge.
        let bar_face = mast - (MAST_HALF_W + 2.0 * BOOM_HALF_FACE) + 0.01;
        let reach = mine
            .iter()
            .filter(|p| p[0] < bar_face)
            .map(|p| p[1])
            .reduce(f32::max)
            .map(|top| top - BOOM_Y);
        (mast, reach)
    }

    /// The two mast centres **read back off the meshed geometry**, in tile-local
    /// metres, at any yaw — the independent measurement [`mast_centres`] is
    /// checked against.
    ///
    /// Only the masts reach the ground: a lamp head sits at `mast_h` and a boom
    /// band starts at [`BOOM_Y`], so `y < 0.5` picks out the two mast boxes and
    /// nothing else. Each contributes twelve vertices at `y = 0`, symmetric about
    /// its own centre, so a group's centroid *is* that centre.
    ///
    /// The two groups are separated without consulting the helper — the masts sit
    /// diametrically opposite through the crossing point, so the sign of the dot
    /// product against the farthest ground vertex splits them — or this would
    /// measure the helper against itself.
    fn meshed_mast_centres(crossing: &LevelCrossing) -> [[f32; 2]; 2] {
        let meshes = build_crossing_meshes(std::slice::from_ref(crossing), None);
        assert_eq!(meshes.len(), 1);
        let ground: Vec<[f32; 2]> = meshes[0]
            .positions
            .iter()
            .filter(|p| p[1] < 0.5)
            .map(|p| [p[0] - crossing.x, p[2] - crossing.z])
            .collect();
        assert_eq!(
            ground.len(),
            24,
            "two mast boxes, twelve ground corners each"
        );

        let axis = *ground
            .iter()
            .max_by(|a, b| (a[0] * a[0] + a[1] * a[1]).total_cmp(&(b[0] * b[0] + b[1] * b[1])))
            .expect("a mast reaches the ground");
        let mut sides: [Vec<[f32; 2]>; 2] = [Vec::new(), Vec::new()];
        for p in &ground {
            let i = usize::from(p[0] * axis[0] + p[1] * axis[1] < 0.0);
            sides[i].push(*p);
        }
        sides.map(|group| {
            assert_eq!(group.len(), 12, "a mast box lost corners to the split");
            let n = group.len() as f32;
            let sum = group
                .iter()
                .fold([0.0f32; 2], |a, p| [a[0] + p[0], a[1] + p[1]]);
            [crossing.x + sum[0] / n, crossing.z + sum[1] / n]
        })
    }

    /// A pair of points equals another pair, in either order, to a millimetre.
    fn same_pair(got: [[f32; 2]; 2], want: [[f32; 2]; 2]) -> bool {
        let near = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).hypot(a[1] - b[1]) < 1e-3;
        (near(got[0], want[0]) && near(got[1], want[1]))
            || (near(got[0], want[1]) && near(got[1], want[0]))
    }

    /// The collider follows the mast, at a yaw where getting the rotation the
    /// wrong way round would show.
    ///
    /// This is the whole safety of the collider channel: `mast_centres` publishes
    /// numbers no capture can see, so nothing but this test says the circle is on
    /// the post rather than across the road from it. A yaw-0 check would pass with
    /// the rotation's sign flipped — the failure this module's siblings have
    /// already paid for twice (`interiors`' door normals).
    #[test]
    fn the_mast_centres_are_where_the_masts_are_meshed() {
        for yaw in [0.0, 0.7, -1.9, 2.6] {
            for kind in [
                CrossingKind::HalfBarrier,
                CrossingKind::MiniBarrier,
                CrossingKind::FullBarrier,
                CrossingKind::Lights,
            ] {
                let c = LevelCrossing {
                    yaw,
                    road_half: Some(5.5),
                    ..crossing(kind, 3)
                };
                let want = meshed_mast_centres(&c);
                let got = mast_centres(&c);
                assert!(
                    same_pair(got, want),
                    "{kind:?} at yaw {yaw}: collider at {got:?}, mast at {want:?}"
                );
            }
        }
    }

    /// Every form gets two mast colliders — `Lights` included, which is the one
    /// that could plausibly be skipped: it has no boom, but it has the same two
    /// masts and lamp heads standing on the same verge.
    #[test]
    fn every_form_has_two_mast_colliders() {
        for kind in [
            CrossingKind::HalfBarrier,
            CrossingKind::MiniBarrier,
            CrossingKind::FullBarrier,
            CrossingKind::Lights,
        ] {
            let c = crossing(kind, 2);
            let [a, b] = mast_centres(&c);
            // Across the road at the verge, along it at the clearance — the two
            // numbers the masts are placed from, in the frame yaw 0 makes the
            // object frame.
            let (_, fallback) = kind.spec();
            let across = fallback + VERGE_M;
            let along = clearance_m(2);
            assert!(
                (a[0] - (c.x + across)).abs() < 1e-4 && (a[1] - (c.z + along)).abs() < 1e-4,
                "{kind:?}: {a:?}"
            );
            assert!(
                (b[0] - (c.x - across)).abs() < 1e-4 && (b[1] - (c.z - along)).abs() < 1e-4,
                "{kind:?}: {b:?}"
            );
        }
    }

    /// A wider road moves the collider out with the mast — the width is carried
    /// through one derivation, so a crossing on an 11 m road cannot get a mast on
    /// the verge and a circle in the carriageway.
    #[test]
    fn the_collider_takes_the_roads_width_too() {
        let narrow = mast_centres(&crossing(CrossingKind::HalfBarrier, 1));
        let wide = mast_centres(&LevelCrossing {
            road_half: Some(5.5),
            ..crossing(CrossingKind::HalfBarrier, 1)
        });
        assert!((narrow[0][0] - 44.0).abs() < 1e-3, "{narrow:?}");
        assert!((wide[0][0] - 46.0).abs() < 1e-3, "{wide:?}");
    }

    /// The table's rule, and the acceptance test for it: a code nobody wrote a
    /// form for places **nothing**, rather than the nearest thing in the table.
    /// `furniture`'s module note states why, and this register makes it sharper —
    /// half of it is codes for installations that do not exist.
    #[test]
    fn an_unnamed_code_places_nothing() {
        for code in [
            "WILO", "VKL", "HAVIO", "EBO", "ADOB", "", "   ", "AHOB2", "overweg",
        ] {
            assert_eq!(crossing_kind(code), None, "{code} should draw nothing");
        }
    }

    /// The two the register uses for "there is nothing here" and "I do not know",
    /// which between them are 2,075 of its 3,917 features. Drawing an
    /// installation on either is the biggest single lie this layer could tell.
    #[test]
    fn geen_and_onbekend_place_nothing() {
        assert_eq!(crossing_kind("Geen"), None);
        assert_eq!(crossing_kind("Onbekend"), None);
        assert_eq!(crossing_kind("GEEN"), None);
    }

    /// The codes that do have a form, so a table edit that drops one is caught.
    #[test]
    fn the_named_codes_keep_their_forms() {
        assert_eq!(crossing_kind("AHOB"), Some(CrossingKind::HalfBarrier));
        assert_eq!(crossing_kind("AHOB-MINI"), Some(CrossingKind::MiniBarrier));
        assert_eq!(crossing_kind("AOB"), Some(CrossingKind::FullBarrier));
        assert_eq!(crossing_kind("AKI"), Some(CrossingKind::Lights));
        assert_eq!(crossing_kind("ali"), Some(CrossingKind::Lights));
    }

    /// A boom lies across the **road**, so the yaw has to come from the road —
    /// and the register's point is on the rail centreline, so the nearest way to
    /// it is always the track.
    ///
    /// This is the one piece of logic in the module that is not borrowed, and its
    /// failure mode is a crossing that looks fine from above and has its booms
    /// down the middle of the railway. The field here holds exactly the two ways
    /// the real case has: a track running east–west and a road crossing it
    /// north–south, with the crossing point on the track.
    #[test]
    fn orientation_comes_from_the_road_not_the_track() {
        let mut field = WayField::empty(400.0);
        // The rail, east–west through the crossing point, and *nearest* to it.
        field.push_line(
            WayFilter::ExcludingRail,
            "rail",
            &[[0.0, 100.0], [400.0, 100.0]],
        );
        // The road, north–south, crossing it at x = 200.
        field.push_line(
            WayFilter::ExcludingRail,
            "residential",
            &[[200.0, 0.0], [200.0, 400.0]],
        );

        let placed = place_crossings_against(
            &field,
            &[CrossingPoint {
                x: 200.0,
                z: 100.0,
                kind: CrossingKind::HalfBarrier,
                tracks: 1,
            }],
        );
        assert_eq!(placed.len(), 1, "the crossing should be placed");

        // Local +z along the road, which runs +z here: yaw 0 (or its half turn).
        let yaw = placed[0].yaw;
        let along = (yaw.sin(), yaw.cos());
        assert!(
            along.0.abs() < 1e-3,
            "the booms took the track's direction, not the road's: yaw {yaw}"
        );
        assert!(along.1.abs() > 0.99, "yaw {yaw} is not along the road");
    }

    /// The same field with the filter off answers the *track*, which is what
    /// makes the test above a test of the filter rather than of the geometry.
    #[test]
    fn without_the_filter_the_track_wins() {
        let mut field = WayField::empty(400.0);
        field.push_line(WayFilter::All, "rail", &[[0.0, 100.0], [400.0, 100.0]]);
        field.push_line(
            WayFilter::All,
            "residential",
            &[[200.0, 0.0], [200.0, 400.0]],
        );
        let (yaw, _) = field
            .direction_and_width(200.0, 100.0, ORIENT_RADIUS_M)
            .unwrap();
        assert!(
            yaw.sin().abs() > 0.99,
            "expected the east–west track, got yaw {yaw}"
        );
    }

    /// Every form emits geometry, indexed inside its own vertex list, in the
    /// group the furniture shares.
    #[test]
    fn every_form_meshes() {
        for kind in [
            CrossingKind::HalfBarrier,
            CrossingKind::MiniBarrier,
            CrossingKind::FullBarrier,
            CrossingKind::Lights,
        ] {
            let meshes = build_crossing_meshes(&[crossing(kind, 2)], None);
            assert_eq!(meshes.len(), 1, "{kind:?}");
            assert_eq!(meshes[0].group, SurfaceGroup::Furniture);
            assert!(!meshes[0].indices.is_empty(), "{kind:?}");
            assert_eq!(meshes[0].positions.len(), meshes[0].normals.len());
            assert_eq!(meshes[0].positions.len(), meshes[0].colors.len());
            for i in &meshes[0].indices {
                assert!((*i as usize) < meshes[0].positions.len(), "{kind:?}");
            }
        }
    }

    /// The byte bound, on the same terms `build_mark_meshes` is held to: a cap
    /// splits the cell into pieces, each piece holds at least one crossing, and
    /// no piece is empty.
    #[test]
    fn the_byte_cap_splits_between_crossings() {
        let many: Vec<LevelCrossing> = (0..12)
            .map(|_| crossing(CrossingKind::HalfBarrier, 2))
            .collect();
        let whole = build_crossing_meshes(&many, None);
        assert_eq!(whole.len(), 1);

        let cap = CROSSING_MAX_BYTES * 3;
        let cut = build_crossing_meshes(&many, Some(cap));
        assert!(cut.len() > 1, "the cap did not split anything");
        for mesh in &cut {
            assert!(!mesh.positions.is_empty(), "an empty piece was emitted");
            let bytes = mesh.positions.len() * VERTEX_BYTES + mesh.indices.len() * INDEX_BYTES;
            assert!(bytes <= cap, "a piece overran the cap: {bytes} > {cap}");
        }
        let cut_verts: usize = cut.iter().map(|m| m.positions.len()).sum();
        assert_eq!(cut_verts, whole[0].positions.len(), "the cut lost geometry");
    }

    /// A cap under one installation's cost still emits it, rather than looping on
    /// empty flushes.
    #[test]
    fn a_cap_below_one_crossing_still_draws_it() {
        let meshes = build_crossing_meshes(&[crossing(CrossingKind::FullBarrier, 4)], Some(1));
        assert_eq!(meshes.len(), 1);
        assert!(!meshes[0].positions.is_empty());
    }

    /// The same rule again, but through the path production takes: a real tile,
    /// whose `streets` features carry their class as a `kind` tag. The test above
    /// drives [`WayFilter::admits`] directly and cannot catch a build that never
    /// reads the tag at all — which would admit every railway silently.
    #[test]
    fn the_tile_side_filter_reads_the_kind_tag() {
        use crate::vector_tiles::synth;

        // A tile carrying one railway and nothing else, running east–west across
        // the middle of it. `tile_m` 400 over a 4096 extent.
        let rail_only = synth::tile(&[synth::tagged_line_layer(
            "streets",
            4096,
            ("kind", "rail"),
            &[(0, 1024), (4095, 1024)],
        )]);
        let tile = TileData::parse(&rail_only).expect("the tile parses");
        let excluding = WayField::build_with(&tile, 400.0, WayFilter::ExcludingRail);
        assert!(
            excluding
                .direction_and_width(200.0, 100.0, ORIENT_RADIUS_M)
                .is_none(),
            "the rail was indexed despite the filter"
        );
        let everything = WayField::build_with(&tile, 400.0, WayFilter::All);
        assert!(
            everything
                .direction_and_width(200.0, 100.0, ORIENT_RADIUS_M)
                .is_some(),
            "the unfiltered field lost the way too, so this proves nothing"
        );
    }

    /// How many booms an installation has comes from its **form**, never from
    /// the register's `aantal_ahob`: the Peizerweg crossing is a single-track
    /// AHOB and reports 15 of them, so that field is an asset tally rather than a
    /// count of what is standing there. [`CrossingPoint`] does not carry it, and
    /// two barrier forms with wildly different tallies mesh identically.
    #[test]
    fn the_boom_count_comes_from_the_form_not_the_register() {
        assert!(CrossingKind::HalfBarrier.boom_span().is_some());
        assert!(CrossingKind::Lights.boom_span().is_none());
        // Same form, same geometry — there is no per-instance boom count to vary.
        let a = build_crossing_meshes(&[crossing(CrossingKind::HalfBarrier, 1)], None);
        let b = build_crossing_meshes(&[crossing(CrossingKind::HalfBarrier, 1)], None);
        assert_eq!(a[0].positions.len(), b[0].positions.len());
        // …and a boom form is strictly more geometry than a lights-only one.
        let lights = build_crossing_meshes(&[crossing(CrossingKind::Lights, 1)], None);
        assert!(a[0].positions.len() > lights[0].positions.len());
    }

    /// A boom stands **up**, not across.
    ///
    /// This is the assertion the module was missing: a boom lying flat is a
    /// crossing that reads as permanently closed to traffic with nothing coming,
    /// and it is what shipped — the bands were stacked along local x at a
    /// constant `BOOM_Y`, ending within half a metre of the road's centreline.
    /// Every existing test passes with the bar in either attitude
    /// (`every_form_meshes` counts triangles, `the_byte_cap_splits_between_crossings`
    /// counts bytes, `the_boom_count_comes_from_the_form_not_the_register` counts
    /// vertices), so nothing but a shot could tell them apart.
    ///
    /// Two facts separate them without having to identify which vertices belong
    /// to which box: nothing stands over the carriageway, and the mesh reaches
    /// the top of the raised bar in y. A `Lights` crossing tops out at its lamp
    /// head, which is what makes the second one a statement about the boom.
    #[test]
    fn a_raised_boom_stands_clear_of_the_road() {
        for kind in [
            CrossingKind::HalfBarrier,
            CrossingKind::MiniBarrier,
            CrossingKind::FullBarrier,
        ] {
            let (mast_h, road_half) = kind.spec();
            // The reach `push_crossing` derives, on its own terms
            // (`crossings.rs:394-397`): a multiple of the half-carriageway, plus
            // the verge the mast stands back by.
            let reach = kind.boom_span().expect("a barrier form has a boom") * road_half + VERGE_M;
            let c = crossing(kind, 2);
            // Yaw 0, so the object's local x is the world's: `push_box`'s
            // rotation is the identity and the across-road axis is `x - c.x`.
            let meshes = build_crossing_meshes(&[c], None);
            assert_eq!(meshes.len(), 1, "{kind:?}");

            let clear = road_half - 0.25;
            let top = meshes[0]
                .positions
                .iter()
                .fold(f32::MIN, |hi, p| hi.max(p[1]));
            for p in &meshes[0].positions {
                assert!(
                    (p[0] - c.x).abs() >= clear,
                    "{kind:?}: a vertex at x {} is over the carriageway (clear {clear} m)",
                    p[0] - c.x
                );
            }
            assert!(
                top >= BOOM_Y + reach - 0.05,
                "{kind:?}: the mesh tops out at {top} m, below a raised boom's \
                 {} m — the bar is still lying flat",
                BOOM_Y + reach
            );

            // The bar stands flush beside its mast, never inside it. Measured on
            // the +side installation, where inboard is −x: the mast's inner face
            // against the bar's outer face.
            let mx = road_half + VERGE_M;
            let mast_inner = mx - MAST_HALF_W;
            let bar_outer = (mx - (MAST_HALF_W + BOOM_HALF_FACE)) + BOOM_HALF_FACE;
            assert!(
                mast_inner - bar_outer >= -1e-4,
                "{kind:?}: the bar overlaps the mast by {} m",
                bar_outer - mast_inner
            );
            // …and the lamp head, not the boom, is what a `Lights` crossing
            // reaches, so the check above cannot be satisfied by the mast.
            assert!(
                BOOM_Y + reach > mast_h + 0.32,
                "{kind:?}: the boom does not clear the lamp head"
            );
        }
    }

    /// A crossing with no boom is untouched by the attitude change, pinned as an
    /// absolute box rather than against a golden file: two masts and two lamp
    /// heads, topping out at the lamp rather than at a bar. It is the control for
    /// the test above — a form that reaches only `mast_h + 0.32` in y is what
    /// makes "the mesh reaches `BOOM_Y + reach`" a statement about the boom.
    #[test]
    fn a_lights_crossing_is_two_masts_and_two_lamps() {
        let (mast_h, road_half) = CrossingKind::Lights.spec();
        assert!(CrossingKind::Lights.boom_span().is_none());
        let c = crossing(CrossingKind::Lights, 2);
        let meshes = build_crossing_meshes(&[c], None);
        // Four boxes, 24 vertices each — no boom bands.
        assert_eq!(meshes[0].positions.len(), 4 * 24);

        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for p in &meshes[0].positions {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        // Across the road, the lamp head's 0.30 m half-width off the mast at
        // `road_half + 0.5`; up, the top of that head; along, the clearance plus
        // the mast's own half-width.
        let across = road_half + VERGE_M + 0.30;
        let along = clearance_m(2) + MAST_HALF_W;
        for (got, want) in [
            (lo[0], c.x - across),
            (hi[0], c.x + across),
            (lo[1], 0.0),
            (hi[1], mast_h + 0.32),
            (lo[2], c.z - along),
            (hi[2], c.z + along),
        ] {
            assert!((got - want).abs() < 1e-4, "bounds moved: {got} != {want}");
        }
    }

    /// The track count only ever moves the two installations apart, and it is
    /// bounded at both ends: zero tracks is a crossing nobody counted, and a
    /// number past [`MAX_TRACKS`] would strew masts over a neighbourhood.
    #[test]
    fn the_track_count_is_bounded_at_both_ends() {
        assert_eq!(clearance_m(0), clearance_m(1));
        assert!(clearance_m(2) > clearance_m(1));
        assert_eq!(clearance_m(u8::MAX), clearance_m(MAX_TRACKS as u8));
        assert!(clearance_m(MAX_TRACKS as u8) < 40.0);
    }

    /// The whole point of the change: a crossing takes its width off the road it
    /// bars, through the path production takes.
    ///
    /// The Peizerweg is OSM `highway = secondary`, which `road_width_m` answers
    /// 11.0 m for — the ribbon this client actually draws. Sized for the old
    /// tabulated 7 m road, both masts stood ~1.5 m inside the kerb and each boom
    /// covered under half the carriageway.
    ///
    /// Driven through `synth` + `WayField::build_with` rather than through
    /// `push_line`, so a build that never reads the tags is caught here as
    /// `the_tile_side_filter_reads_the_kind_tag` catches it for the filter.
    #[test]
    fn a_wide_road_gets_a_wide_crossing() {
        use crate::vector_tiles::synth;

        // One `secondary` road running north–south down the middle of a 400 m
        // cell, and the crossing point on it.
        let tile = TileData::parse(&synth::tile(&[synth::tagged_line_layer(
            "streets",
            4096,
            ("kind", "secondary"),
            &[(2048, 0), (2048, 4095)],
        )]))
        .expect("the tile parses");
        let roads = WayField::build_with(&tile, 400.0, WayFilter::ExcludingRail);
        let placed = place_crossings_against(
            &roads,
            &[CrossingPoint {
                x: 200.0,
                z: 100.0,
                kind: CrossingKind::HalfBarrier,
                tracks: 1,
            }],
        );
        assert_eq!(placed.len(), 1);
        assert_eq!(
            placed[0].road_half,
            Some(5.5),
            "the width did not come off the tile"
        );

        // `place_crossings` leaves the yaw sign unresolved, and `across` reads
        // in the object frame, so normalise to yaw 0 — the geometry is the same
        // under a half turn by construction.
        let crossing = LevelCrossing {
            yaw: 0.0,
            ..placed[0]
        };
        let (mast, reach) = across(&crossing);
        assert!(
            (mast - 6.0).abs() < 1e-3,
            "the mast should stand 0.5 m outside an 11 m road's kerb, got {mast}"
        );
        // A half barrier bars the entry half, so the bar is as long as the mast
        // stands out: lowered, its tip would meet the centreline.
        assert!(
            (reach.expect("a half barrier has a boom") - 6.0).abs() < 1e-3,
            "the bar should be 6.0 m long, so it meets the centreline when \
             lowered, got {reach:?}"
        );
    }

    /// The same derivation the other way round: the commonest small form was
    /// sized for a 3.2 m path and stands on cycleways two metres wide.
    #[test]
    fn a_narrow_road_gets_a_narrow_crossing() {
        let mut field = WayField::empty(400.0);
        field.push_line(
            WayFilter::ExcludingRail,
            "cycleway",
            &[[200.0, 0.0], [200.0, 400.0]],
        );
        let placed = place_crossings_against(
            &field,
            &[CrossingPoint {
                x: 200.0,
                z: 100.0,
                kind: CrossingKind::MiniBarrier,
                tracks: 1,
            }],
        );
        assert_eq!(placed[0].road_half, Some(1.1), "cycleway is 2.2 m wide");
        let (mast, reach) = across(&LevelCrossing {
            yaw: 0.0,
            ..placed[0]
        });
        assert!(
            (mast - 1.6).abs() < 1e-3,
            "the mini form should get narrower, not just the big forms wider: {mast}"
        );
        assert!(
            (reach.expect("a boom") - 1.6).abs() < 1e-3,
            "the bar should shrink with the mast, to 1.6 m: {reach:?}"
        );
    }

    /// A full barrier bars the **whole** road, so its bar is long enough to
    /// reach the far kerb rather than the centreline — the one form whose reach
    /// is not the same multiple of the half-width.
    #[test]
    fn a_full_barrier_reaches_the_far_kerb() {
        let crossing = LevelCrossing {
            road_half: Some(5.5),
            ..crossing(CrossingKind::FullBarrier, 1)
        };
        let (mast, reach) = across(&crossing);
        assert!((mast - 6.0).abs() < 1e-3, "{mast}");
        // 2 × 5.5 + the verge: from a mast 6.0 m out, that is the far kerb at
        // −5.5 m once the bar is lowered.
        assert!(
            (reach.expect("a boom") - 11.5).abs() < 1e-3,
            "the bar should be 11.5 m long, enough to cross the whole road, got {reach:?}"
        );
    }

    /// Where the field cannot answer a width — an untagged way, a kind
    /// `road_class` refuses, a tunnel — every form is placed exactly where it
    /// was before the width was carried. This is the compatibility half, and it
    /// is what makes the change safe on a tile with no usable `kind` tags at all.
    #[test]
    fn an_unknown_width_falls_back_to_the_old_numbers() {
        for (kind, mast_x, reach) in [
            (CrossingKind::HalfBarrier, 4.0, Some(4.0)),
            (CrossingKind::MiniBarrier, 2.1, Some(2.1)),
            // 7.5 against the old table's 7.6: the 10 cm is the accepted cost of
            // deriving the reach from the half-width.
            (CrossingKind::FullBarrier, 4.0, Some(7.5)),
            (CrossingKind::Lights, 4.0, None),
        ] {
            let (mast, drawn) = across(&crossing(kind, 1));
            assert!((mast - mast_x).abs() < 1e-3, "{kind:?}: mast at {mast}");
            match reach {
                Some(r) => {
                    let drawn = drawn.expect("a boom");
                    assert!((drawn - r).abs() < 1e-3, "{kind:?}: bar {drawn} m long");
                }
                None => assert!(drawn.is_none(), "{kind:?} should have no boom"),
            }
        }
    }

    /// A tunnel and an unclassified kind are both "no width known", not zero — a
    /// zero would put both masts on the centreline and collapse the booms.
    #[test]
    fn a_kind_with_no_road_class_answers_no_width() {
        let mut field = WayField::empty(400.0);
        // Admitted by the filter (it is not rail), refused by `road_class`.
        field.push_line(
            WayFilter::ExcludingRail,
            "elevator",
            &[[200.0, 0.0], [200.0, 400.0]],
        );
        let placed = place_crossings_against(
            &field,
            &[CrossingPoint {
                x: 200.0,
                z: 100.0,
                kind: CrossingKind::HalfBarrier,
                tracks: 1,
            }],
        );
        assert_eq!(placed.len(), 1, "the way still orients the crossing");
        assert_eq!(placed[0].road_half, None);
    }

    /// A crossing with no road near it is dropped rather than drawn at a guessed
    /// angle — the module note's reason, and the thing that keeps the
    /// `level_crossings` count honest about what is on screen.
    #[test]
    fn a_crossing_with_no_road_is_dropped() {
        let mut field = WayField::empty(400.0);
        field.push_line(
            WayFilter::ExcludingRail,
            "rail",
            &[[0.0, 100.0], [400.0, 100.0]],
        );
        let placed = place_crossings_against(
            &field,
            &[CrossingPoint {
                x: 200.0,
                z: 100.0,
                kind: CrossingKind::HalfBarrier,
                tracks: 1,
            }],
        );
        assert!(placed.is_empty());
    }
}
