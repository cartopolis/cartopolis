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
mod tests;
