//! The buoys and beacons standing on the national waterways, as Rijkswaterstaat
//! surveyed them: shape, colour pattern, topmark and the colour of the light.
//!
//! No Bevy: marks in, [`SurfaceMesh`]es out. The fetching half is
//! `systems::nav_marks`, and `docs/notes/navigation-marks.md` has the endpoint,
//! the licence and the measurements.
//!
//! # What this is for
//!
//! The only thing standing on open water in this client until now was the
//! *inferred* moored boats of [`crate::moorings`], whose own module note is
//! explicit about being a guess read off the shape of a polygon. A buoy is
//! surveyed, individually, with the colour that says which side of it to pass —
//! so a river holds the objects that are really in it rather than nothing at all.
//!
//! # Colour is the content, so the palette bends here
//!
//! [`crate::furniture`] states the rule the rest of this group follows: the map
//! is pale cartographic paper and a saturated bench is the loudest thing on
//! screen. A navigation mark is the one form in the group where the colour *is*
//! the datum — a red can and a green cone mean opposite things and differ in
//! nothing else at 40 m — so [`MarkColour`] is a set of muted signal colours
//! rather than the furniture greys. They are still knocked well back from the
//! real paint, which is fluorescent.
//!
//! # Drop, never substitute
//!
//! Every parser here is total and answers `None` for a value it has no form for,
//! the same rule `surveyed.rs` states for its `PROP_KINDS`. Two consequences are
//! deliberate and are not defects:
//!
//! * **A mark with no readable body colour is dropped**, not drawn grey. The
//!   colour is the mark's meaning; a grey buoy asserts a mark that says nothing.
//!   Three of 260 sampled fixed marks are `Niet toegewezen` and lose nothing by
//!   going undrawn.
//! * **A topmark with no readable colour is dropped, and the mark is kept.** The
//!   body is the mark; the topmark is a qualifier on it.
//!
//! # What is deliberately not here
//!
//! * **The lights lighting.** The register gives a colour, a character (`Iso`,
//!   `Fl`, `Q`, …) and a period, and this paints the lantern in the colour and
//!   stops. The group is one shared `StandardMaterial` lit by vertex colour with
//!   no per-object emissive anywhere in it; a flashing light is a second material
//!   or an emitter, and its own ticket.
//! * **Height.** `obj_hoogte` is `0,00000` on every fixed mark in the country, so
//!   a light tower is drawn as the same post as a groyne mark. The tall-landmark
//!   lane is the client's `systems::tall_structures` and takes its heights from
//!   OSM.
//! * **Non-marks.** The fixed collection also registers verge lighting, sign
//!   floodlighting and shore cabinets. Those are not navigation marks, and the
//!   BGT already surveys street lighting column by column
//!   (`systems::surveyed`) — drawing them from here too would stand two lampposts
//!   in one place. [`FixedForm::parse`] answers `None` for them.

use crate::furniture::{paint_multiplier, part, push_box, MeshBuf, Tone, BOX_BYTES};
use crate::vector_tiles::{palette, SurfaceGroup, SurfaceMesh, INDEX_BYTES, VERTEX_BYTES};

/// Hard ceiling on marks per streamed cell, on the same terms as
/// [`MAX_FURNITURE_PER_CELL`](crate::furniture::MAX_FURNITURE_PER_CELL): one
/// command buffer must not be handed an unbounded pile of triangles. The densest
/// z14 cell measured is the Waal at Nijmegen with 43, so this is headroom rather
/// than a working limit, and truncation is logged rather than silent.
pub const MAX_MARKS_PER_CELL: usize = 400;

/// Sides on every round form here. Eight is where a 1.4 m buoy stops reading as
/// a box and before the vertex count starts to matter — a mark is up to a dozen
/// of these.
const SIDES: usize = 8;

// ─── The vocabularies ────────────────────────────────────────────────────────

/// A colour the register can name on a mark, its topmark or its light.
///
/// The register writes them in Dutch and in mixed case (`Rood`, but `wit` when it
/// is the second band of `Rood/wit repeterend`), so [`MarkColour::parse`] is
/// case-insensitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkColour {
    Red,
    Green,
    Yellow,
    White,
    Black,
    Grey,
    /// Only ever a *light* colour, and only on fixed marks (`licht_kl`).
    Amber,
}

impl MarkColour {
    /// The register's name for one colour, or `None` for anything else —
    /// including its two "nothing here" sentinels, `X` and `Niet toegewezen`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "rood" => Self::Red,
            "groen" => Self::Green,
            "geel" => Self::Yellow,
            "wit" => Self::White,
            "zwart" => Self::Black,
            "grijs" => Self::Grey,
            "amber" => Self::Amber,
            _ => return None,
        })
    }

    /// The sRGB this colour is painted in.
    ///
    /// Muted signal colours: recognisable as red-versus-green at a glance and
    /// still a step off the paper the rest of the map is drawn on. See the module
    /// note for why this layer gets to be more saturated than the furniture
    /// beside it.
    #[must_use]
    pub fn rgb(self) -> u32 {
        match self {
            Self::Red => 0xB2_43_3C,
            Self::Green => 0x3D_78_4C,
            Self::Yellow => 0xD5_B4_3C,
            Self::White => 0xE9_E5_DB,
            Self::Black => 0x38_38_36,
            Self::Grey => 0x9A_98_93,
            Self::Amber => 0xCF_90_33,
        }
    }
}

/// A mark's paint: one to three horizontal bands, listed **top to bottom**.
///
/// `obj_kleur` writes them as `A`, `A/B`, `A/B/A` or `A/B repeterend` over
/// {Rood, Groen, Geel, Wit, Zwart, Grijs}, so this is parsed by splitting on `/`
/// rather than out of a lookup table with an entry per observed string — a table
/// would be 13 rows for the floating collection and 11 more for the fixed one,
/// and would answer nothing for the 14th.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColourPattern {
    /// Top band first. Empty means the register named nothing this reader knows.
    pub bands: Vec<MarkColour>,
    /// `… repeterend`: the bands repeat up the body rather than dividing it once.
    pub repeating: bool,
}

impl ColourPattern {
    /// Parse an `obj_kleur` / `tt_kleur` value. An unreadable band is dropped;
    /// an unreadable *whole* value yields an empty pattern, which every caller
    /// treats as "do not draw this part".
    #[must_use]
    pub fn parse(value: &str) -> Self {
        let trimmed = value.trim();
        let (body, repeating) = match trimmed.to_ascii_lowercase().strip_suffix(" repeterend") {
            Some(_) => (&trimmed[..trimmed.len() - " repeterend".len()], true),
            None => (trimmed, false),
        };
        let bands: Vec<MarkColour> = body.split('/').filter_map(MarkColour::parse).collect();
        Self { bands, repeating }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    /// The bands as they are actually painted.
    ///
    /// A repeating two-colour pattern is expanded to four alternating bands: a
    /// spar buoy in `Rood/wit repeterend` really does carry several turns of each,
    /// and drawing it as one red half over one white half is a different mark.
    #[must_use]
    fn painted(&self) -> Vec<MarkColour> {
        if self.repeating && self.bands.len() == 2 {
            return vec![self.bands[0], self.bands[1], self.bands[0], self.bands[1]];
        }
        self.bands.clone()
    }
}

/// The shape of a floating mark (`obj_vorm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkShape {
    /// `spar` — a slender pole, and by a wide margin the commonest buoy on the
    /// Dutch waterways (702 of 998 sampled).
    Spar,
    /// `stomp` — a can: a flat-topped drum.
    Can,
    /// `spits` — conical.
    Conical,
    /// `bol` — spherical.
    Spherical,
    /// `Pilaar` — a pillar buoy: a wide float carrying a slim column.
    Pillar,
}

impl MarkShape {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "spar" => Self::Spar,
            "stomp" => Self::Can,
            "spits" => Self::Conical,
            "bol" => Self::Spherical,
            "pilaar" => Self::Pillar,
            _ => return None,
        })
    }
}

/// What a fixed mark is (`naut_funct`), reduced to the three forms this client
/// can honestly draw.
///
/// The register's fourteen values collapse to three because `obj_hoogte` is zero
/// on every one of them — with no height there is nothing to tell a light tower
/// from a light post, so claiming the difference in geometry would be inventing
/// it. What the reduction does *not* do is fold in the values that are not marks
/// at all; those are dropped. See the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedForm {
    /// `Kribbaken` — the mark on the head of a groyne, and the commonest fixed
    /// object on a Dutch river (129 of 260 sampled).
    GroyneMark,
    /// An unlit beacon: `Bakenlijn`, `Havenopstand (Onverlicht)`.
    Beacon,
    /// A post carrying a light: `Oeverlicht`, `Havenlicht`, `Lichtopstand`,
    /// `Lichtenlijn`, `Sectorlicht`, `Sperlicht`, `Lichttoren (vuurtoren)`.
    LightPost,
}

impl FixedForm {
    /// Classify a `naut_funct` value.
    ///
    /// `None` covers two different things on purpose: a value from a newer
    /// register than this build, and a value that is not a navigation mark —
    /// `Bermverlichting` and `Bordverlichting` are verge and sign lighting,
    /// `Aanstraalverlichting` is floodlighting and `Walkast` is a shore cabinet.
    /// The BGT surveys street lighting column by column already, so drawing these
    /// would put two lampposts on one spot.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "kribbaken" => Self::GroyneMark,
            "bakenlijn" | "havenopstand (onverlicht)" => Self::Beacon,
            "oeverlicht"
            | "havenlicht"
            | "lichtopstand"
            | "lichtenlijn"
            | "sectorlicht"
            | "sperlicht"
            | "lichttoren (vuurtoren)" => Self::LightPost,
            _ => return None,
        })
    }
}

/// Floating or fixed, and which form of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    Floating(MarkShape),
    Fixed(FixedForm),
}

/// The shape carried on a staff above the mark (`tt_toptek` on a floating mark,
/// `v_toptek` on a fixed one — the register uses two different keys for the same
/// vocabulary).
///
/// This is the part of a mark that says *what it marks* rather than which side to
/// pass, so the two-cone forms are kept apart even though they differ by a few
/// centimetres of geometry: two cones point-to-point is an isolated danger and
/// two cones base-to-base is a safe-water mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topmark {
    Cylinder,
    ConeUp,
    ConeDown,
    Sphere,
    /// `Liggend kruis` — a recumbent cross, drawn as two crossed bars.
    Cross,
    TwoConesUp,
    TwoConesDown,
    /// `2 Kegels, punten naar elkaar` — points toward each other.
    TwoConesInward,
    /// `2 Kegels punten van elkaar af` — points away from each other. Note the
    /// register writes this one without the comma the others carry.
    TwoConesOutward,
    ConeOverSphere,
    CylinderOverSphere,
}

impl Topmark {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "cilinder" => Self::Cylinder,
            "kegel, punt naar boven" => Self::ConeUp,
            "kegel, punt naar beneden" => Self::ConeDown,
            "bol" => Self::Sphere,
            "liggend kruis" => Self::Cross,
            "2 kegels, punten naar boven" => Self::TwoConesUp,
            "2 kegels, punten naar beneden" => Self::TwoConesDown,
            "2 kegels, punten naar elkaar" => Self::TwoConesInward,
            "2 kegels punten van elkaar af" => Self::TwoConesOutward,
            "kegel boven bol" => Self::ConeOverSphere,
            "cilinder boven bol" => Self::CylinderOverSphere,
            _ => return None,
        })
    }
}

/// Everything about a mark except where it stands.
///
/// Split from [`NavMark`] because the streaming half holds these in lat/lng
/// through the cache and only projects them into a cell's metre frame at mesh
/// time, exactly as the crop parcels do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkForm {
    pub kind: MarkKind,
    /// The body's paint. Never empty — a mark whose colour did not parse is
    /// dropped rather than built.
    pub body: ColourPattern,
    /// The topmark, and its own paint. `None` when the register named none, when
    /// this build has no form for the one it named, or when the topmark's colour
    /// did not parse.
    pub topmark: Option<(Topmark, ColourPattern)>,
    /// The colour of the light this mark shows, where it shows one. Only the
    /// colour: see the module note on what is out of scope.
    pub light: Option<MarkColour>,
}

/// One placed mark, in the tessellator's tile-local metre frame (origin at the
/// cell's north-west corner, +x east, +z south, **water surface at y = 0**).
#[derive(Debug, Clone, PartialEq)]
pub struct NavMark {
    pub x: f32,
    pub z: f32,
    pub form: MarkForm,
}

// ─── Geometry ────────────────────────────────────────────────────────────────

/// One piece of a body of revolution: a frustum from `y0` to `y1`, radius `r0` to
/// `r1`. A cylinder has `r0 == r1`, a cone has `r1 == 0`.
#[derive(Debug, Clone, Copy)]
struct Segment {
    y0: f32,
    y1: f32,
    r0: f32,
    r1: f32,
}

const fn seg(y0: f32, y1: f32, r0: f32, r1: f32) -> Segment {
    Segment { y0, y1, r0, r1 }
}

/// The forms, in metres, measured against the real objects rather than stylised.
///
/// **A floating body straddles y = 0 because that is where the water surface
/// is**, the same deliberate choice [`crate::furniture`]'s `BOAT` makes and for
/// the same reason: a buoy resting *on* the water reads as a toy in a bath.
mod form {
    use super::{seg, Segment};

    /// A spar: a 0.44 m pole standing 2.2 m out of the water.
    pub const SPAR: [Segment; 1] = [seg(-0.70, 2.20, 0.22, 0.20)];
    /// A can: a flat-topped drum, 1.4 m across.
    pub const CAN: [Segment; 1] = [seg(-0.45, 1.15, 0.70, 0.70)];
    /// A cone, on a short cylindrical float so the waterline is not the apex of
    /// nothing.
    pub const CONICAL: [Segment; 2] = [seg(-0.45, 0.15, 0.70, 0.70), seg(0.15, 1.55, 0.70, 0.06)];
    /// A sphere, drawn as two frusta — round enough at 1.4 m across, and a third
    /// of the vertices of a real sphere.
    pub const SPHERICAL: [Segment; 2] = [seg(-0.40, 0.35, 0.42, 0.72), seg(0.35, 1.10, 0.72, 0.42)];
    /// A pillar buoy: a wide float carrying a slim column.
    pub const PILLAR: [Segment; 2] = [seg(-0.55, 0.45, 0.85, 0.80), seg(0.45, 2.60, 0.26, 0.22)];
    /// A groyne mark: a slim post. It stands on the groyne, which this client has
    /// no geometry for, so its foot is the waterline.
    pub const GROYNE: [Segment; 1] = [seg(0.0, 3.00, 0.12, 0.10)];
    /// An unlit beacon.
    pub const BEACON: [Segment; 1] = [seg(0.0, 3.50, 0.14, 0.11)];
    /// A post carrying a light.
    pub const LIGHT_POST: [Segment; 1] = [seg(0.0, 4.00, 0.15, 0.12)];
}

impl MarkKind {
    /// The body of this form.
    fn segments(self) -> &'static [Segment] {
        match self {
            Self::Floating(MarkShape::Spar) => &form::SPAR,
            Self::Floating(MarkShape::Can) => &form::CAN,
            Self::Floating(MarkShape::Conical) => &form::CONICAL,
            Self::Floating(MarkShape::Spherical) => &form::SPHERICAL,
            Self::Floating(MarkShape::Pillar) => &form::PILLAR,
            Self::Fixed(FixedForm::GroyneMark) => &form::GROYNE,
            Self::Fixed(FixedForm::Beacon) => &form::BEACON,
            Self::Fixed(FixedForm::LightPost) => &form::LIGHT_POST,
        }
    }
}

/// Height of the staff between a body's top and its topmark.
const STAFF_H: f32 = 0.35;
const STAFF_R: f32 = 0.055;
/// The lantern that a registered light colour is painted on.
const LANTERN_H: f32 = 0.30;
const LANTERN_R: f32 = 0.17;

/// What one [`SIDES`]-gon frustum costs to upload: side faces are flat-shaded and
/// unshared, and each cap is a fan.
const PRISM_BYTES: usize =
    (SIDES * 4 + SIDES * 2) * VERTEX_BYTES + (SIDES * 6 + (SIDES - 2) * 6) * INDEX_BYTES;

/// The most one mark can cost: three body bands over two segments, a staff, a
/// two-band topmark over two segments, its two crossed bars, and a lantern.
///
/// An upper bound rather than a measurement, because the byte cut has to be taken
/// *before* a mark is built or a piece could exceed the budget it was cut for.
const MARK_MAX_BYTES: usize = 12 * PRISM_BYTES + 2 * BOX_BYTES;

/// Merge a cell's marks into the furniture group's meshes, each no larger than
/// `max_bytes`.
///
/// [`SurfaceGroup::Furniture`] and not a group of its own, which is the decision
/// that makes this layer nearly free: marks inherit the group's shared material,
/// its altitude fade (`FURNITURE_FADE_START`..`FURNITURE_FADE_END`) and its
/// Layers-panel toggle without a line of client code for any of the three.
#[must_use]
pub fn build_mark_meshes(marks: &[NavMark], max_bytes: Option<usize>) -> Vec<SurfaceMesh> {
    if marks.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = MeshBuf::default();
    for mark in marks {
        if mark.form.body.is_empty() {
            continue;
        }
        // Flush before adding, and only with something to flush — a piece must
        // hold at least one mark, or a cap under one mark's cost would emit empty
        // meshes forever. Same rule as `build_furniture_meshes`.
        if let Some(max) = max_bytes {
            if buf.bytes() + MARK_MAX_BYTES > max && !buf.positions.is_empty() {
                out.push(std::mem::take(&mut buf).finish(SurfaceGroup::Furniture));
            }
        }
        push_mark(&mut buf, mark);
    }
    if !buf.positions.is_empty() {
        out.push(buf.finish(SurfaceGroup::Furniture));
    }
    out
}

fn push_mark(buf: &mut MeshBuf, mark: &NavMark) {
    let segs = mark.form.kind.segments();
    let top = segs.last().map_or(0.0, |s| s.y1);
    push_banded(buf, mark.x, mark.z, segs, &mark.form.body);

    let mut carry = top;
    if let Some((topmark, colours)) = &mark.form.topmark {
        push_prism(
            buf,
            mark.x,
            mark.z,
            carry,
            carry + STAFF_H,
            STAFF_R,
            STAFF_R,
            tint(palette::FURNITURE_METAL),
        );
        carry += STAFF_H;
        carry = push_topmark(buf, mark.x, mark.z, carry, *topmark, colours);
    }
    if let Some(light) = mark.form.light {
        push_prism(
            buf,
            mark.x,
            mark.z,
            carry,
            carry + LANTERN_H,
            LANTERN_R,
            LANTERN_R * 0.85,
            tint(light.rgb()),
        );
    }
}

/// Draw one topmark and answer the height its top reached, so a lantern can sit
/// on it.
fn push_topmark(
    buf: &mut MeshBuf,
    x: f32,
    z: f32,
    base: f32,
    topmark: Topmark,
    colours: &ColourPattern,
) -> f32 {
    const R: f32 = 0.30;
    const H: f32 = 0.58;
    // Two cones or a cone over a sphere are half-height each, so every topmark
    // occupies the same span whatever it is made of.
    let half = H * 0.5;
    let segs: Vec<Segment> = match topmark {
        Topmark::Cylinder => vec![seg(base, base + H, R, R)],
        Topmark::ConeUp => vec![seg(base, base + H, R, 0.0)],
        Topmark::ConeDown => vec![seg(base, base + H, 0.0, R)],
        Topmark::Sphere => vec![
            seg(base, base + half, R * 0.55, R),
            seg(base + half, base + H, R, R * 0.55),
        ],
        Topmark::TwoConesUp => vec![
            seg(base, base + half, R, 0.0),
            seg(base + half, base + H, R, 0.0),
        ],
        Topmark::TwoConesDown => vec![
            seg(base, base + half, 0.0, R),
            seg(base + half, base + H, 0.0, R),
        ],
        // Points toward each other: the lower cone points up, the upper points
        // down, and they meet at the waist.
        Topmark::TwoConesInward => vec![
            seg(base, base + half, R, 0.0),
            seg(base + half, base + H, 0.0, R),
        ],
        // Points away from each other: the bases meet in the middle.
        Topmark::TwoConesOutward => vec![
            seg(base, base + half, 0.0, R),
            seg(base + half, base + H, R, 0.0),
        ],
        Topmark::ConeOverSphere => vec![
            seg(base, base + half * 0.5, R * 0.5, R * 0.75),
            seg(base + half * 0.5, base + half, R * 0.75, R * 0.5),
            seg(base + half, base + H, R, 0.0),
        ],
        Topmark::CylinderOverSphere => vec![
            seg(base, base + half * 0.5, R * 0.5, R * 0.75),
            seg(base + half * 0.5, base + half, R * 0.75, R * 0.5),
            seg(base + half, base + H, R, R),
        ],
        // Two bars crossed at a right angle, laid flat against the sky rather
        // than turned edge-on — a recumbent cross has to read from any bearing,
        // and this layer has no bearing to orient against.
        Topmark::Cross => {
            let rgb = colours.painted().first().map_or(0x38_38_36, |c| c.rgb());
            let t = tint(rgb);
            for yaw in [0.0_f32, std::f32::consts::FRAC_PI_2] {
                let (sn, cs) = yaw.sin_cos();
                let p = part([0.0, base + half, 0.0], [R * 1.15, 0.05, 0.05], Tone::Paint);
                push_box(buf, x, z, (sn, cs), &p, t);
            }
            return base + H;
        }
    };
    push_banded(buf, x, z, &segs, colours);
    base + H
}

/// Draw a body of revolution in its colour bands, top band first.
///
/// The bands are laid across the whole span rather than one per segment, so a
/// pillar buoy's float and column are painted continuously — the register
/// describes the *mark*, not its parts.
fn push_banded(buf: &mut MeshBuf, x: f32, z: f32, segs: &[Segment], colours: &ColourPattern) {
    let painted = colours.painted();
    if painted.is_empty() || segs.is_empty() {
        return;
    }
    let (lo, hi) = (segs[0].y0, segs[segs.len() - 1].y1);
    let n = painted.len() as f32;
    for (i, colour) in painted.iter().enumerate() {
        // Top band first: band 0 is the slice just under `hi`.
        let band_hi = hi - (hi - lo) * (i as f32) / n;
        let band_lo = hi - (hi - lo) * (i as f32 + 1.0) / n;
        let t = tint(colour.rgb());
        for s in segs {
            let y0 = s.y0.max(band_lo);
            let y1 = s.y1.min(band_hi);
            if y1 <= y0 + 1e-3 {
                continue;
            }
            let span = s.y1 - s.y0;
            let radius = |y: f32| {
                if span.abs() < 1e-6 {
                    s.r0
                } else {
                    s.r0 + (s.r1 - s.r0) * ((y - s.y0) / span)
                }
            };
            push_prism(buf, x, z, y0, y1, radius(y0), radius(y1), t);
        }
    }
}

/// The vertex tint that turns the furniture group's shared base colour into
/// `rgb`, in the linear space the shader multiplies in.
fn tint(rgb: u32) -> [f32; 4] {
    let m = paint_multiplier(rgb);
    [m[0], m[1], m[2], 1.0]
}

/// Emit one [`SIDES`]-gon frustum: flat-shaded sides plus a cap at each end.
///
/// Sides are unshared for the same reason [`push_box`] does not share them — a
/// 20 cm buoy shaded off averaged normals loses the facets that are all there is
/// to see of it at this size.
#[allow(clippy::too_many_arguments)]
fn push_prism(
    buf: &mut MeshBuf,
    cx: f32,
    cz: f32,
    y0: f32,
    y1: f32,
    r0: f32,
    r1: f32,
    tint: [f32; 4],
) {
    if y1 <= y0 || (r0 <= 0.0 && r1 <= 0.0) {
        return;
    }
    let step = std::f32::consts::TAU / SIDES as f32;
    let ring = |r: f32, i: usize| {
        let a = step * i as f32;
        (cx + r * a.cos(), cz + r * a.sin())
    };

    for i in 0..SIDES {
        let j = (i + 1) % SIDES;
        let (ax, az) = ring(r0, i);
        let (bx, bz) = ring(r0, j);
        let (cxx, czz) = ring(r1, j);
        let (dx, dz) = ring(r1, i);
        // Outward normal of the quad, from the face's own cross product so a cone
        // is lit as a cone rather than as a cylinder.
        let u = [bx - ax, 0.0, bz - az];
        let v = [dx - ax, y1 - y0, dz - az];
        let mut n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len > 1e-6 {
            n = [n[0] / len, n[1] / len, n[2] / len];
        } else {
            n = [0.0, 1.0, 0.0];
        }
        let base = buf.positions.len() as u32;
        for p in [[ax, y0, az], [bx, y0, bz], [cxx, y1, czz], [dx, y1, dz]] {
            buf.positions.push(p);
            buf.normals.push(n);
            buf.colors.push(tint);
        }
        buf.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // Caps. A degenerate one (a cone's apex) is skipped rather than emitted as a
    // fan of slivers.
    for (r, y, up) in [(r0, y0, false), (r1, y1, true)] {
        if r <= 1e-4 {
            continue;
        }
        let base = buf.positions.len() as u32;
        let normal = if up {
            [0.0, 1.0, 0.0]
        } else {
            [0.0, -1.0, 0.0]
        };
        for i in 0..SIDES {
            let (px, pz) = ring(r, i);
            buf.positions.push([px, y, pz]);
            buf.normals.push(normal);
            buf.colors.push(tint);
        }
        for i in 1..SIDES - 1 {
            let (a, b) = (i as u32, i as u32 + 1);
            if up {
                buf.indices.extend([base, base + a, base + b]);
            } else {
                buf.indices.extend([base, base + b, base + a]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(kind: MarkKind, body: &str) -> NavMark {
        NavMark {
            x: 10.0,
            z: 20.0,
            form: MarkForm {
                kind,
                body: ColourPattern::parse(body),
                topmark: None,
                light: None,
            },
        }
    }

    /// Every `obj_kleur` value observed over 998 floating and 260 fixed marks on
    /// 2026-08-30 (`docs/notes/navigation-marks.md` has the method), driven off
    /// the literal strings the service returns. The parser is a split rather than
    /// a table precisely so the 15th value works too, and this is what pins that.
    #[test]
    fn every_observed_colour_pattern_parses() {
        let observed = [
            ("Rood", vec![MarkColour::Red], false),
            ("Groen", vec![MarkColour::Green], false),
            ("Geel", vec![MarkColour::Yellow], false),
            ("Wit", vec![MarkColour::White], false),
            ("Zwart", vec![MarkColour::Black], false),
            ("Grijs", vec![MarkColour::Grey], false),
            (
                "Groen/wit repeterend",
                vec![MarkColour::Green, MarkColour::White],
                true,
            ),
            (
                "Rood/wit repeterend",
                vec![MarkColour::Red, MarkColour::White],
                true,
            ),
            (
                "Rood/groen repeterend",
                vec![MarkColour::Red, MarkColour::Green],
                true,
            ),
            (
                "Zwart/wit repeterend",
                vec![MarkColour::Black, MarkColour::White],
                true,
            ),
            (
                "Zwart/geel",
                vec![MarkColour::Black, MarkColour::Yellow],
                false,
            ),
            (
                "Geel/zwart",
                vec![MarkColour::Yellow, MarkColour::Black],
                false,
            ),
            ("Rood/wit", vec![MarkColour::Red, MarkColour::White], false),
            (
                "Groen/rood",
                vec![MarkColour::Green, MarkColour::Red],
                false,
            ),
            (
                "Rood/groen",
                vec![MarkColour::Red, MarkColour::Green],
                false,
            ),
            (
                "Groen/rood/groen",
                vec![MarkColour::Green, MarkColour::Red, MarkColour::Green],
                false,
            ),
            (
                "Rood/groen/rood",
                vec![MarkColour::Red, MarkColour::Green, MarkColour::Red],
                false,
            ),
            (
                "Geel/zwart/geel",
                vec![MarkColour::Yellow, MarkColour::Black, MarkColour::Yellow],
                false,
            ),
            (
                "Zwart/geel/zwart",
                vec![MarkColour::Black, MarkColour::Yellow, MarkColour::Black],
                false,
            ),
            (
                "Rood/wit/rood",
                vec![MarkColour::Red, MarkColour::White, MarkColour::Red],
                false,
            ),
        ];
        for (raw, bands, repeating) in observed {
            let parsed = ColourPattern::parse(raw);
            assert_eq!(parsed.bands, bands, "{raw}");
            assert_eq!(parsed.repeating, repeating, "{raw}");
        }
    }

    /// The register's two "nothing here" sentinels, and a value from a future
    /// register. All three must produce an empty pattern rather than a colour.
    #[test]
    fn the_absent_sentinels_produce_no_colour() {
        for raw in ["X", "Niet toegewezen", "#", "", "Magenta"] {
            assert!(
                ColourPattern::parse(raw).is_empty(),
                "{raw} produced a colour"
            );
        }
    }

    #[test]
    fn every_observed_shape_and_form_parses() {
        for (raw, want) in [
            ("spar", MarkShape::Spar),
            ("stomp", MarkShape::Can),
            ("spits", MarkShape::Conical),
            ("bol", MarkShape::Spherical),
            ("Pilaar", MarkShape::Pillar),
        ] {
            assert_eq!(MarkShape::parse(raw), Some(want), "{raw}");
        }
        assert_eq!(MarkShape::parse("X"), None);
        assert_eq!(MarkShape::parse("Niet toegewezen"), None);
        // A shape from a newer register is dropped, never clamped onto the last
        // known form: a mark drawn as the wrong shape says the wrong thing.
        assert_eq!(MarkShape::parse("Toren"), None);

        for (raw, want) in [
            ("Kribbaken", FixedForm::GroyneMark),
            ("Bakenlijn", FixedForm::Beacon),
            ("Havenopstand (Onverlicht)", FixedForm::Beacon),
            ("Oeverlicht", FixedForm::LightPost),
            ("Havenlicht", FixedForm::LightPost),
            ("Lichtopstand", FixedForm::LightPost),
            ("Lichtenlijn", FixedForm::LightPost),
            ("Sectorlicht", FixedForm::LightPost),
            ("Sperlicht", FixedForm::LightPost),
            ("Lichttoren (vuurtoren)", FixedForm::LightPost),
        ] {
            assert_eq!(FixedForm::parse(raw), Some(want), "{raw}");
        }
    }

    /// The register carries installations that are not navigation marks, and the
    /// BGT already surveys street lighting object by object — drawing these too
    /// would stand two lampposts on one spot.
    #[test]
    fn the_fixed_collections_non_marks_are_dropped() {
        for raw in [
            "Bermverlichting",
            "Bordverlichting",
            "Aanstraalverlichting",
            "Walkast",
        ] {
            assert_eq!(FixedForm::parse(raw), None, "{raw} was drawn as a mark");
        }
    }

    #[test]
    fn every_observed_topmark_parses() {
        for (raw, want) in [
            ("Cilinder", Topmark::Cylinder),
            ("Kegel, punt naar boven", Topmark::ConeUp),
            ("Kegel, punt naar beneden", Topmark::ConeDown),
            ("Bol", Topmark::Sphere),
            ("Liggend kruis", Topmark::Cross),
            ("2 Kegels, punten naar boven", Topmark::TwoConesUp),
            ("2 Kegels, punten naar beneden", Topmark::TwoConesDown),
            ("2 Kegels, punten naar elkaar", Topmark::TwoConesInward),
            // The register writes this one without the comma the others carry;
            // matching it as if it had one drops the safe-water topmark.
            ("2 Kegels punten van elkaar af", Topmark::TwoConesOutward),
            ("Kegel boven bol", Topmark::ConeOverSphere),
            ("Cilinder boven bol", Topmark::CylinderOverSphere),
        ] {
            assert_eq!(Topmark::parse(raw), Some(want), "{raw}");
        }
        assert_eq!(Topmark::parse("X"), None);
        assert_eq!(Topmark::parse("Niet toegewezen"), None);
    }

    /// A repeating two-colour spar really does carry several turns of each; one
    /// red half over one white half is a different mark.
    #[test]
    fn a_repeating_pattern_paints_more_than_two_bands() {
        let p = ColourPattern::parse("Rood/wit repeterend");
        assert_eq!(p.painted().len(), 4);
        assert_eq!(ColourPattern::parse("Rood/wit").painted().len(), 2);
        assert_eq!(ColourPattern::parse("Geel/zwart/geel").painted().len(), 3);
    }

    #[test]
    fn a_mark_becomes_a_furniture_mesh() {
        let meshes = build_mark_meshes(
            &[mark(MarkKind::Floating(MarkShape::Conical), "Groen")],
            None,
        );
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].group, SurfaceGroup::Furniture);
        assert!(!meshes[0].indices.is_empty());
        assert_eq!(meshes[0].positions.len(), meshes[0].normals.len());
        assert_eq!(meshes[0].positions.len(), meshes[0].colors.len());
        for i in &meshes[0].indices {
            assert!((*i as usize) < meshes[0].positions.len());
        }
    }

    /// A floating body straddles the water surface, and a fixed one stands on it.
    /// Getting this wrong puts a buoy on top of the water like a bath toy, or
    /// sinks a beacon.
    #[test]
    fn a_buoy_straddles_the_waterline_and_a_beacon_stands_on_it() {
        for shape in [
            MarkShape::Spar,
            MarkShape::Can,
            MarkShape::Conical,
            MarkShape::Spherical,
            MarkShape::Pillar,
        ] {
            let segs = MarkKind::Floating(shape).segments();
            assert!(segs[0].y0 < 0.0, "{shape:?} floats above the water");
            assert!(segs[segs.len() - 1].y1 > 0.5, "{shape:?} has no freeboard");
        }
        for f in [
            FixedForm::GroyneMark,
            FixedForm::Beacon,
            FixedForm::LightPost,
        ] {
            assert!(MarkKind::Fixed(f).segments()[0].y0 >= 0.0, "{f:?} is sunk");
        }
    }

    /// A mark whose colour did not parse says nothing, so it is not drawn — the
    /// alternative is a grey buoy, which is a mark that means something else.
    #[test]
    fn a_mark_with_no_colour_is_not_drawn() {
        let meshes = build_mark_meshes(
            &[mark(MarkKind::Floating(MarkShape::Spar), "Niet toegewezen")],
            None,
        );
        assert!(meshes.is_empty());
    }

    /// Every topmark has to produce geometry, or a form is in the vocabulary and
    /// invisible on the water — which reads exactly like a mark that has none.
    #[test]
    fn every_topmark_draws_something() {
        for topmark in [
            Topmark::Cylinder,
            Topmark::ConeUp,
            Topmark::ConeDown,
            Topmark::Sphere,
            Topmark::Cross,
            Topmark::TwoConesUp,
            Topmark::TwoConesDown,
            Topmark::TwoConesInward,
            Topmark::TwoConesOutward,
            Topmark::ConeOverSphere,
            Topmark::CylinderOverSphere,
        ] {
            let mut m = mark(MarkKind::Floating(MarkShape::Spar), "Rood");
            let bare = build_mark_meshes(std::slice::from_ref(&m), None)[0]
                .indices
                .len();
            m.form.topmark = Some((topmark, ColourPattern::parse("Zwart")));
            let with = build_mark_meshes(&[m], None)[0].indices.len();
            assert!(with > bare, "{topmark:?} drew nothing");
        }
    }

    /// The light is a painted lantern and nothing more — but it does have to be
    /// there, or the 27 % of floating marks that carry one lose the only part of
    /// the register that says so.
    #[test]
    fn a_registered_light_adds_a_lantern() {
        let mut m = mark(MarkKind::Fixed(FixedForm::LightPost), "Wit");
        let bare = build_mark_meshes(std::slice::from_ref(&m), None)[0]
            .indices
            .len();
        m.form.light = Some(MarkColour::Green);
        let lit = build_mark_meshes(&[m], None)[0].indices.len();
        assert!(lit > bare);
    }

    /// The byte cap is what keeps a fairway's worth of marks off the single-asset
    /// upload cliff, so it has to actually cut — and cutting must not lose one.
    #[test]
    fn the_byte_cap_cuts_a_cell_into_pieces() {
        let marks: Vec<NavMark> = (0..40)
            .map(|_| mark(MarkKind::Floating(MarkShape::Can), "Rood/wit repeterend"))
            .collect();
        let whole = build_mark_meshes(&marks, None);
        let split = build_mark_meshes(&marks, Some(MARK_MAX_BYTES * 4));
        assert_eq!(whole.len(), 1);
        assert!(split.len() > 1, "the cap should have cut it up");
        let tris = |m: &[SurfaceMesh]| m.iter().map(|x| x.indices.len()).sum::<usize>();
        assert_eq!(tris(&whole), tris(&split), "cutting lost triangles");
    }

    /// Every band of a pattern has to reach the mesh, or a three-banded mark is
    /// drawn as a one-banded one — which is a different mark.
    #[test]
    fn each_band_paints_its_own_slice() {
        let meshes = build_mark_meshes(
            &[mark(MarkKind::Floating(MarkShape::Spar), "Geel/zwart/geel")],
            None,
        );
        let colours: std::collections::HashSet<[u32; 3]> = meshes[0]
            .colors
            .iter()
            .map(|c| [c[0].to_bits(), c[1].to_bits(), c[2].to_bits()])
            .collect();
        assert_eq!(colours.len(), 2, "yellow/black/yellow needs two tints");
    }

    /// Two colours that draw the same are a distinction the register is carrying
    /// for nothing.
    #[test]
    fn every_colour_has_its_own_tone() {
        use MarkColour::*;
        let all = [Red, Green, Yellow, White, Black, Grey, Amber];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.rgb(), b.rgb(), "{a:?} and {b:?} share a tone");
            }
        }
    }
}
