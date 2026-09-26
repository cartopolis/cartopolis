//! Road surfaces: which OSM class a street is, how wide it is, and what its
//! surface looks like up close.
//!
//! The raster path (`systems::vector_tiles::render_mvt`) draws streets as flat
//! coloured strokes, which is right for a map you look at from above. Once the
//! geometry path puts a road under the avatar's feet, a flat stroke is exactly
//! what a road does *not* look like: real tarmac has grain, edge lines and lane
//! markings, and their spacing is what makes a road read as a road at a grazing
//! angle.
//!
//! So the surface is a **texture**, and it is generated here rather than shipped
//! as an asset:
//!
//! * The markings have to line up with the geometry. The ribbon in
//!   `systems::vector_tiles::tessellate_roads` runs `u` from 0 to 1 straight
//!   across the carriageway, so a centre line is at `u = 0.5` *whatever* the road
//!   is wide — a photographic tile could never keep that registration.
//! * `v` is metres along the road divided by [`RoadClass::texture_length_m`], so
//!   dash lengths are in real metres and stay right under any magnification.
//! * It costs one 64×256 buffer per class at startup instead of a download, and
//!   it survives the wasm32 target with no asset plumbing (ADR-033).
//!
//! Everything here is pure CPU and deterministic — same class in, same pixels
//! out — so it runs on the tile worker pool or the main thread indifferently.

/// Texture width in texels: the across-the-road axis. Narrow on purpose — `u`
/// only ever spans one carriageway, and the markings are analytically
/// anti-aliased, so resolution buys nothing here.
const TEX_W: u32 = 64;

/// Texture height in texels: the along-the-road axis, covering
/// [`RoadClass::texture_length_m`] metres.
const TEX_H: u32 = 256;

/// What a street is made of. Coarser than the OSM `kind` because this is a
/// *material*: one class = one texture = one Bevy material, and every extra
/// material is another mesh per streamed cell (see the batching note in
/// `systems::map_geometry`). Width varies per `kind` inside a class — a
/// residential street and a tertiary road share tarmac and share nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoadClass {
    /// Motorway/trunk: lane dashes both carriageways, solid median and shoulders.
    Motorway,
    /// **One direction** of a fast road: a dual carriageway's separately-mapped
    /// half, or a slip road. No median — the oncoming traffic is a different
    /// feature, several metres away. See [`StreetFlags`].
    Carriageway,
    /// Primary/secondary: solid edge lines, dashed centre line.
    Major,
    /// Tertiary down to service roads: unmarked tarmac with wheel tracks.
    Minor,
    /// Footway, cycleway, track: gravel/grit, no markings.
    Path,
    /// Railway: ballast, sleepers and two steel rails.
    Rail,
}

impl RoadClass {
    /// How many metres of road one vertical repeat of the texture covers. This is
    /// the scale every dash length below is expressed against.
    #[must_use]
    pub fn texture_length_m(self) -> f32 {
        match self {
            Self::Motorway => 24.0,
            Self::Carriageway => 24.0,
            Self::Major => 24.0,
            Self::Minor => 12.0,
            Self::Path => 4.0,
            Self::Rail => 4.8,
        }
    }

    /// The flat surface colour for this class, used when road textures are
    /// switched off (`DataLayers::roads_textured`): the texture's base tarmac/grit
    /// tone, without markings. Linear-space rgb, as `Color::srgb` expects.
    #[must_use]
    pub fn flat_color(self) -> [f32; 3] {
        let rgb = self.base_tone();
        [
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
        ]
    }

    /// [`flat_color`](Self::flat_color) as `0xRRGGBB`, for the raster basemap.
    /// Above `map_geometry`'s image altitude the basemap is the only ground
    /// drawn, so it paints each road in the tone its mesh is built from — the
    /// cartographic near-white it used before made every road vanish into the
    /// paper the moment the meshes went away.
    #[must_use]
    pub fn flat_rgb(self) -> u32 {
        let [r, g, b] = self.base_tone();
        (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
    }

    fn base_tone(self) -> [u8; 3] {
        match self {
            Self::Motorway | Self::Carriageway | Self::Major => TARMAC_DARK,
            Self::Minor => TARMAC_LIGHT,
            Self::Path => GRIT,
            Self::Rail => BALLAST,
        }
    }

    /// Where the tyres run, as positions across the carriageway (0 = left kerb,
    /// 1 = right kerb).
    ///
    /// Traffic polishes the two ribbons under a vehicle's wheels and leaves the
    /// strip between them and the gutters beside them coarse. On a road that has
    /// carried traffic for a season this is the most visible variation in the
    /// surface — it is what you see picked out by a low sun or by headlights, and
    /// it is the reason a road photographs as two light bands rather than as one
    /// flat tone.
    ///
    /// Positions are per *lane*, at roughly the track width of an ordinary car
    /// against the lane width. A path has no vehicles and a railway has no tyres,
    /// so both are empty and the [`surface`](RoadTexture::surface) map leaves
    /// them uniform.
    fn wheel_paths(self) -> &'static [f32] {
        match self {
            // Two lanes each way between the shoulder and the median, matching
            // the marking layout above — a polished track outside its own lane
            // line is a track in the hard shoulder, where no traffic runs.
            Self::Motorway => &[0.185, 0.245, 0.345, 0.405, 0.595, 0.655, 0.755, 0.815],
            // One direction, two lanes: shoulder to the right of the `kantstreep`
            // at u = 0.75, so both tracks sit inboard of it.
            Self::Carriageway => &[0.10, 0.22, 0.42, 0.54],
            // One lane each way.
            Self::Major => &[0.18, 0.34, 0.66, 0.82],
            Self::Minor => &[0.24, 0.40, 0.60, 0.76],
            Self::Path | Self::Rail => &[],
        }
    }

    /// Surface roughness for the material. Tarmac is not quite matte — a wet-ish
    /// sheen at a grazing angle is most of what separates it from painted ground.
    #[must_use]
    pub fn perceptual_roughness(self) -> f32 {
        match self {
            Self::Motorway | Self::Carriageway | Self::Major => 0.78,
            Self::Minor => 0.84,
            Self::Rail => 0.9,
            Self::Path => 0.95,
        }
    }
}

/// The Shortbread `streets` attributes that change how a street is *drawn*, as
/// opposed to what it is called.
///
/// A `kind` alone is not enough to know how wide a street is, because OSM maps a
/// dual carriageway as **two** ways — each tagged `motorway`, each `oneway`, a
/// median apart. Widening both to the full 22 m of a motorway laid two slabs of
/// tarmac over each other with a metre or two of daylight between their centre
/// lines: the doubled markings and the wide dark blob at an interchange. Ramps
/// (`link`) are the same mistake in the other direction — a single-lane slip road
/// drawn as wide as the motorway it leaves is most of what turns a junction into
/// a puddle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreetFlags {
    /// Traffic runs one way. On the fast classes this means "one half of a dual
    /// carriageway"; on a residential street it means nothing about the width, so
    /// only the fast classes read it.
    pub oneway: bool,
    /// Traffic runs one way **against** the way's own direction.
    ///
    /// Folded into `oneway` until 2026-09-19 and thrown away, which was fine
    /// while nothing across the carriageway was asymmetric. A hard shoulder is:
    /// it is on the right of the *traffic*, and the ribbon's `u` axis runs left
    /// to right of the *geometry*. Those are opposite on a reversed way, so
    /// without this a motorway's shoulder lands in the fast lane on roughly half
    /// the features — worse than having no shoulder at all, and in a way that
    /// looks like a texture bug rather than a data one.
    pub oneway_reverse: bool,
    /// A slip road or ramp (OSM's `*_link`).
    pub link: bool,
}

impl StreetFlags {
    /// One direction of traffic on a fast road: a dual carriageway's half, or a
    /// ramp (which is one-way in practice whether or not it is tagged so).
    #[must_use]
    fn single_direction(self) -> bool {
        self.oneway || self.link
    }
}

/// Map a Shortbread `streets` `kind` to a surface class. `None` means "do not
/// draw as geometry": `subway` is underground, and an unknown kind is better
/// left to the raster tile underneath than guessed at in 3D.
#[must_use]
pub fn road_class(kind: &str, flags: StreetFlags) -> Option<RoadClass> {
    let fast = matches!(kind, "motorway" | "trunk" | "primary" | "secondary");
    if fast && flags.single_direction() {
        return Some(RoadClass::Carriageway);
    }
    Some(match kind {
        "motorway" | "trunk" => RoadClass::Motorway,
        "primary" | "secondary" => RoadClass::Major,
        "tertiary" | "residential" | "unclassified" | "living_street" | "service"
        | "pedestrian" => RoadClass::Minor,
        "footway" | "path" | "cycleway" | "steps" | "track" => RoadClass::Path,
        "rail" | "light_rail" | "tram" => RoadClass::Rail,
        _ => return None,
    })
}

/// Does a paving survey describe this `streets` kind as a real shape, so the
/// constant-width ribbon for it must not also be drawn over a surveyed cell?
///
/// Deliberately beside [`road_class`] so the two cannot drift, exactly as
/// [`is_rail_kind`] is: it names the kinds that map to [`RoadClass::Path`],
/// **plus `pedestrian`**, which maps to [`RoadClass::Minor`] because that is what
/// it looks like where nobody has surveyed it — a street you can walk down, wide
/// enough to carry a delivery van, and cased at its kerb.
///
/// That difference is the whole reason this is not a test on the class. A rule
/// keyed on `RoadClass::Path` alone left every pedestrian street and every
/// pedestrian square drawing a 5 m tarmac ribbon with wheel tracks and kerbs on
/// top of the paving the survey had already laid there — the Grote Markt in
/// Groningen is mapped as fourteen separate `pedestrian` ways, so it came out as
/// a scatter of tarmac stubs across the square (2026-09-11).
#[must_use]
pub fn surveyed_paving_supersedes(kind: &str) -> bool {
    matches!(
        kind,
        "footway" | "path" | "cycleway" | "steps" | "track" | "pedestrian"
    )
}

/// Is this `streets` kind track rather than road?
///
/// Deliberately beside [`road_class`] so the two cannot drift: it names the same
/// three kinds that map to [`RoadClass::Rail`], **plus `subway`**, which
/// `road_class` answers `None` for because it is underground. That difference is
/// the point of a separate predicate — "do not draw this in 3D" and "this is not
/// a road" are different questions, and a caller asking the second one about a
/// subway wants `true`.
///
/// The caller is [`crate::crossings`]: a level crossing's registered point sits
/// *on the track centreline*, so the nearest way to it is always the railway, and
/// the boom lies across the road.
#[must_use]
pub fn is_rail_kind(kind: &str) -> bool {
    matches!(kind, "rail" | "light_rail" | "tram" | "subway")
}

/// Carriageway width in **metres** for a `streets` kind — the real width of the
/// thing, not a cartographic stroke weight. The raster path's widths are in
/// pixels at the source tile's scale and deliberately exaggerated so a street is
/// visible on a zoomed-out map; geometry has no such excuse, and a 20 m wide
/// residential street is obvious the moment you stand on it.
///
/// The [`StreetFlags`] are what keep the fast classes honest: the base numbers
/// are for a road that carries **both** directions, which a `oneway` motorway or
/// a ramp does not.
#[must_use]
pub fn road_width_m(kind: &str, flags: StreetFlags) -> f32 {
    // A ramp is one or two lanes plus a hard strip, whatever it ramps off.
    if flags.link {
        return match kind {
            "motorway" | "trunk" => 8.0,
            "primary" | "secondary" => 7.0,
            _ => 6.0,
        };
    }
    // Half of a dual carriageway: the lanes of one direction, its hard shoulder,
    // and none of the median — that belongs to the gap between the two features.
    if flags.oneway {
        match kind {
            "motorway" => return 12.0,
            "trunk" => return 10.0,
            "primary" => return 8.0,
            "secondary" => return 7.0,
            _ => {}
        }
    }
    match kind {
        "motorway" => 22.0,
        "trunk" => 18.0,
        "primary" => 14.0,
        "secondary" => 11.0,
        "tertiary" => 9.0,
        "residential" | "unclassified" => 7.0,
        "living_street" => 6.0,
        "pedestrian" => 5.0,
        "service" => 4.0,
        "track" => 3.0,
        "cycleway" => 2.2,
        "steps" | "footway" => 1.8,
        "path" => 1.5,
        "rail" | "light_rail" | "tram" => RAIL_BED_M,
        _ => 6.0,
    }
}

/// A generated surface: mip-0 RGBA8, row-major, `width` across the road.
pub struct RoadTexture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The same texels again as a glTF-style metallic-roughness map: **G is
    /// roughness, B is metallic**, R and A unused. Sampled linearly, never as
    /// sRGB — these are material coefficients, not a colour.
    ///
    /// This is what stops a street being one uniform finish. Three things vary
    /// across it, and each is something you can see on a real road at a grazing
    /// angle: traffic **polishes the wheel paths**, so the two ribbons a car's
    /// tyres run along are glossier than the coarse tarmac between and beside
    /// them; **paint is smoother than the surface it is painted on**, which is
    /// why a wet line flares before the road around it does; and a **rail head is
    /// bare steel**, i.e. metallic and near-smooth, against ballast and timber
    /// that are neither.
    ///
    /// It is generated in the same loop as the colour, from the same coverage
    /// values, so the two cannot drift out of registration — a marking is exactly
    /// as smooth as it is white.
    pub surface: Vec<u8>,
}

/// One painted band running along the road, positioned in `u`.
struct Marking {
    /// Centre of the band across the road, 0 = left kerb, 1 = right kerb.
    u: f32,
    /// Half the band's width, in the same `u` units.
    half_u: f32,
    color: [u8; 3],
    /// `(painted metres, period metres)`; `None` for a continuous line. The
    /// period must divide [`RoadClass::texture_length_m`] or the pattern will
    /// jump at the texture's wrap.
    dash: Option<(f32, f32)>,
}

impl Marking {
    /// How smooth this band is, against the tarmac's 1.0.
    ///
    /// Derived from the colour rather than carried as a field, so the two dozen
    /// `solid`/`dashed` call sites above stay as short as they are: there are
    /// exactly two materials painted onto a road here, traffic paint and the
    /// steel of a rail head, and the colour already says which.
    fn roughness(&self) -> f32 {
        if self.color == STEEL {
            0.22
        } else {
            0.62
        }
    }

    /// Only a rail is metal. Paint on tarmac is a dielectric like the tarmac.
    fn metallic(&self) -> f32 {
        if self.color == STEEL {
            1.0
        } else {
            0.0
        }
    }

    fn solid(u: f32, half_u: f32, color: [u8; 3]) -> Self {
        Self {
            u,
            half_u,
            color,
            dash: None,
        }
    }

    fn dashed(u: f32, half_u: f32, color: [u8; 3], on_m: f32, period_m: f32) -> Self {
        Self {
            u,
            half_u,
            color,
            dash: Some((on_m, period_m)),
        }
    }
}

/// The painted part of a lane-division dash, and its repeat, in metres.
///
/// **The Dutch standard is 3 m of paint and a 9 m gap** outside the built-up area
/// and on motorways (`nl.wikipedia.org/wiki/Wegmarkering`; the dash is 15 cm
/// wide). Until 2026-09-19 the fast classes painted 4 m on the same 12 m repeat —
/// the right rhythm with a third too much paint — and `Major` painted 3 m on an
/// 8 m repeat, which is no standard at all: 3-and-5 is neither the rural 3-and-9
/// nor the urban 1-and-3.
///
/// One constant for all three because they are one rule. The inside-the-town
/// pattern (1 m and 3 m) is not reachable from here: a texture is per
/// [`RoadClass`] and the class does not know whether it is in a town.
///
/// **The period must divide [`RoadClass::texture_length_m`] exactly**, or the
/// pattern jumps at every texture wrap — a stutter down the road at a fixed
/// interval, which reads as a tiling artifact rather than as a marking fault.
/// `dash_period_tiles_the_texture` pins it, and moving `Major` to this period is
/// why its repeat went from 16 m to 24 m.
/// Where the `kantstreep` — the solid line between the running lanes and the hard
/// shoulder — sits, as a fraction in from the *outer* edge of the tarmac.
///
/// A Dutch hard shoulder is 3 m. These are that 3 m over the two modelled widths
/// [`road_width_m`] gives the fast classes: 22 m for a bidirectional motorway and
/// 12 m for one carriageway of a dual one. Stated as the two ratios rather than
/// computed, because a texture is built per class and has no width to divide by —
/// `carriageway_shoulder_is_three_metres` is what keeps the two in step.
const KANTSTREEP_U_22: f32 = 3.0 / 22.0;
const KANTSTREEP_U_12: f32 = 3.0 / 12.0;

/// Half-width of an edge line in `u`. A Dutch `kantstreep` is 20 cm, wider than
/// the 15 cm of a lane division, and it is the line that carries at a distance.
const EDGE_HALF_U: f32 = 0.009;

/// Standard gauge, between the inner faces of the rails.
///
/// 1435 mm in the Netherlands as in most of the world
/// (`nl.wikipedia.org/wiki/Normaalspoor`). Until 2026-09-19 the two rails were
/// drawn at `u` 0.30 and 0.70 of a 3.2 m bed, which is a **1.28 m** gauge — 11 %
/// narrow, and narrow in the one direction a person can check against a
/// photograph without a tape measure, because the sleeper ends stick out past the
/// rails by a fixed amount and the proportion is what the eye reads.
const RAIL_GAUGE_M: f32 = 1.435;

/// Width of a rail head, near enough. What separates rail *centres* from the
/// gauge, which is measured between their inner faces.
const RAIL_HEAD_M: f32 = 0.07;

/// The ballast bed a single track sits on, and the width the ribbon is drawn at.
///
/// A concrete sleeper is 2.5 m and the ballast shoulder runs about 0.55 m past
/// each end. The old 3.2 m was close enough to this that the *bed* never looked
/// wrong — only the gauge inside it did.
const RAIL_BED_M: f32 = 3.6;

/// Half the rail-centre separation, as a fraction of the bed. Derived, so the
/// gauge cannot drift from the bed width the ribbon is actually drawn at —
/// `the_track_is_standard_gauge` is what checks the arithmetic.
const RAIL_HALF_U: f32 = (RAIL_GAUGE_M + RAIL_HEAD_M) / (2.0 * RAIL_BED_M);

/// Half-width of the drawn rail head in `u`. Wider than the steel itself: at this
/// scale the band stands for the rail, its fastenings and the shadow under it.
const RAIL_HEAD_HALF_U: f32 = 0.030;

const DASH_ON_M: f32 = 3.0;
const DASH_PERIOD_M: f32 = 12.0;

/// Traffic paint, and the two surface tones the classes are built from. Dimmer
/// than real-world paint on purpose: this sits under the same sun as everything
/// else, and a 0xFF white line blooms into a stripe of glare at midday.
const PAINT_WHITE: [u8; 3] = [0xE2, 0xE0, 0xD6];
const TARMAC_DARK: [u8; 3] = [0x4A, 0x4A, 0x4E];
const TARMAC_LIGHT: [u8; 3] = [0x59, 0x58, 0x59];
const GRIT: [u8; 3] = [0x9A, 0x8C, 0x74];
const BALLAST: [u8; 3] = [0x77, 0x71, 0x69];
const SLEEPER: [u8; 3] = [0x4C, 0x40, 0x36];
const STEEL: [u8; 3] = [0x8E, 0x8C, 0x8A];

/// What each class is painted with: base tarmac, grain strength, and the bands
/// running along it.
///
/// Split out of [`road_texture`] so the table can be *inspected* rather than only
/// rasterised — `dash_period_tiles_the_texture` reads the dash periods straight
/// off it. A rule stated in a doc comment and checked by nothing is how `Major`
/// came to be measured against no standard at all.
fn markings_of(class: RoadClass) -> ([u8; 3], f32, Vec<Marking>) {
    match class {
        // **Two lanes each way with a hard shoulder outside them**, laid out on
        // the real 22 m: 3 m shoulder, 2x3.5 m lanes, a 2 m median, and the mirror
        // of that. Both carriageways in one ribbon, so it is symmetric and needs
        // no travel direction.
        //
        // It used to be three lanes each way and *no shoulder* — the outermost
        // line sat at the very edge of the tarmac, so the width a shoulder should
        // occupy was painted as a running lane. A hard shoulder is the most
        // recognisable thing about a motorway from any angle: a wide unmarked
        // strip outside a bright continuous line (the `kantstreep`). Compared
        // against a photograph of the A7 on the Afsluitdijk, 2026-09-19.
        RoadClass::Motorway => (
            TARMAC_DARK,
            0.055,
            vec![
                Marking::solid(KANTSTREEP_U_22, EDGE_HALF_U, PAINT_WHITE),
                Marking::solid(1.0 - KANTSTREEP_U_22, EDGE_HALF_U, PAINT_WHITE),
                Marking::solid(0.455, 0.006, PAINT_WHITE),
                Marking::solid(0.545, 0.006, PAINT_WHITE),
                Marking::dashed(0.295, 0.005, PAINT_WHITE, DASH_ON_M, DASH_PERIOD_M),
                Marking::dashed(0.705, 0.005, PAINT_WHITE, DASH_ON_M, DASH_PERIOD_M),
            ],
        ),
        // One direction of a dual carriageway (or a ramp), on the real 12 m: a
        // narrow strip against the median, two 3.5 m lanes, the `kantstreep`, and
        // a 3 m hard shoulder. No median of its own — that is the daylight
        // between this feature and the one carrying the other direction.
        //
        // **This one is asymmetric, and that is why `oneway_reverse` is now
        // carried.** The shoulder is on the right of the *traffic*; `u` runs left
        // to right of the *geometry*; on a reversed way those are opposite, and
        // `tessellate_roads` flips `u` so the shoulder stays where a driver would
        // find it.
        RoadClass::Carriageway => (
            TARMAC_DARK,
            0.055,
            vec![
                Marking::solid(0.025, 0.006, PAINT_WHITE),
                Marking::solid(1.0 - KANTSTREEP_U_12, EDGE_HALF_U, PAINT_WHITE),
                Marking::dashed(0.317, 0.006, PAINT_WHITE, DASH_ON_M, DASH_PERIOD_M),
            ],
        ),
        RoadClass::Major => (
            TARMAC_DARK,
            0.06,
            vec![
                Marking::solid(0.045, 0.009, PAINT_WHITE),
                Marking::solid(0.955, 0.009, PAINT_WHITE),
                Marking::dashed(0.500, 0.008, PAINT_WHITE, DASH_ON_M, DASH_PERIOD_M),
            ],
        ),
        // Unmarked, but not featureless: two darker polished wheel tracks are
        // what tells you which way the traffic runs.
        RoadClass::Minor => (
            TARMAC_LIGHT,
            0.075,
            vec![
                Marking::solid(0.30, 0.085, [0x4F, 0x4E, 0x4F]),
                Marking::solid(0.70, 0.085, [0x4F, 0x4E, 0x4F]),
            ],
        ),
        RoadClass::Path => (GRIT, 0.16, Vec::new()),
        // The two rails at **standard gauge**, positioned from the real
        // dimensions rather than from a pair of round numbers that looked right.
        RoadClass::Rail => (
            BALLAST,
            0.20,
            vec![
                Marking::solid(0.5 - RAIL_HALF_U, RAIL_HEAD_HALF_U, STEEL),
                Marking::solid(0.5 + RAIL_HALF_U, RAIL_HEAD_HALF_U, STEEL),
            ],
        ),
    }
}

/// Build the mip-0 surface for one class.
///
/// Pixel centres are sampled analytically against each band, so an edge line
/// narrower than a texel still lands as a soft grey line instead of aliasing in
/// and out of existence along the road.
#[must_use]
pub fn road_texture(class: RoadClass) -> RoadTexture {
    let (base, grain, markings) = markings_of(class);

    let len_m = class.texture_length_m();
    let wheels = class.wheel_paths();
    let mut rgba = Vec::with_capacity((TEX_W * TEX_H * 4) as usize);
    let mut surface = Vec::with_capacity((TEX_W * TEX_H * 4) as usize);

    for y in 0..TEX_H {
        // Metres along the road at this row's *span*, for the dash coverage.
        let v0 = y as f32 / TEX_H as f32 * len_m;
        let v1 = (y + 1) as f32 / TEX_H as f32 * len_m;

        for x in 0..TEX_W {
            let u0 = x as f32 / TEX_W as f32;
            let u1 = (x + 1) as f32 / TEX_W as f32;

            let mut rgb = shade(base, grain, x, y);
            // The surface map is built beside the colour from the same coverage
            // values — see `RoadTexture::surface`. 1.0 is the class's own
            // roughness; the wheel paths take it down, paint and steel take it
            // down further, and only a rail head is ever metallic.
            let mut rough = 1.0 - wheel_polish(&wheels, u0, u1);
            // The same grain that dapples the colour, at a quarter weight. Without
            // it a polished wheel path is a smooth band with a hard edge, which
            // reads as a decal rather than as wear.
            rough *= 1.0 - grain * 0.25 * (hash2(x, y) * 2.0 - 1.0);
            let mut metal = 0.0;

            // Sleepers first: the rails are painted on top of them.
            if class == RoadClass::Rail {
                let cover = sleeper_coverage(u0, u1, v0, v1);
                if cover > 0.0 {
                    rgb = mix(rgb, shade(SLEEPER, 0.10, x, y), cover);
                    // Creosoted timber, which is if anything rougher than the
                    // ballast around it.
                    rough = rough * (1.0 - cover) + cover;
                }
            }

            for m in &markings {
                let across = span_overlap(u0, u1, m.u - m.half_u, m.u + m.half_u) / (u1 - u0);
                if across <= 0.0 {
                    continue;
                }
                let along = match m.dash {
                    None => 1.0,
                    Some((on_m, period_m)) => dash_coverage(v0, v1, on_m, period_m),
                };
                let cover = across * along;
                if cover > 0.0 {
                    // Paint wears; keep the class's grain so a line is not a
                    // perfectly flat slab against a noisy surface.
                    rgb = mix(rgb, shade(m.color, grain * 0.4, x, y), cover);
                    // Thermoplastic is a smoother surface than the tarmac under
                    // it, and a rail head is smoother again and made of metal.
                    // Blended by the same coverage as the colour, so an
                    // analytically anti-aliased line edge is anti-aliased here too
                    // rather than stepping a texel early.
                    rough = rough * (1.0 - cover) + m.roughness() * cover;
                    metal = metal * (1.0 - cover) + m.metallic() * cover;
                }
            }

            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 0xFF]);
            surface.extend_from_slice(&[
                0xFF,
                to_u8(rough.clamp(MIN_ROUGHNESS, 1.0)),
                to_u8(metal.clamp(0.0, 1.0)),
                0xFF,
            ]);
        }
    }

    RoadTexture {
        width: TEX_W,
        height: TEX_H,
        rgba,
        surface,
    }
}

/// Lowest roughness any texel of a road may reach.
///
/// A floor rather than a preference. `perceptual_roughness` multiplies this map,
/// and `systems::wet_surfaces` lowers that scalar again in the rain — so a texel
/// allowed to reach 0 would be a perfect mirror on a wet night, which is a bug
/// that only appears in one weather at one time of day.
const MIN_ROUGHNESS: f32 = 0.18;

/// How polished this texel is by traffic, 0 (untouched) to 1 (in a wheel path).
///
/// A raised cosine rather than a band, so a wheel path fades into the coarse
/// tarmac beside it the way a real one does. `HALF_TRACK_U` is half the width of
/// one polished ribbon in `u`.
fn wheel_polish(wheels: &[f32], u0: f32, u1: f32) -> f32 {
    /// Half-width of a polished ribbon, across the road.
    const HALF_TRACK_U: f32 = 0.055;
    /// How far the roughness falls inside one. Kept modest: a wheel path is
    /// smoother than the road, not a different material.
    const DEPTH: f32 = 0.34;

    let u = (u0 + u1) * 0.5;
    let mut best: f32 = 0.0;
    for &centre in wheels {
        let d = (u - centre).abs() / HALF_TRACK_U;
        if d < 1.0 {
            // cos² falloff: 1 at the centre, 0 and flat-tangent at the edge.
            let t = ((1.0 - d) * std::f32::consts::FRAC_PI_2).sin();
            best = best.max(t * t);
        }
    }
    best * DEPTH
}

/// Quantise a 0..1 coefficient to a texel.
fn to_u8(v: f32) -> u8 {
    (v * 255.0 + 0.5) as u8
}

/// Sleeper bands: 0.35 m of timber every 0.6 m, stopping short of the ballast
/// shoulders. Expressed in metres so it tiles with the class's own scale.
fn sleeper_coverage(u0: f32, u1: f32, v0: f32, v1: f32) -> f32 {
    let across = span_overlap(u0, u1, 0.10, 0.90) / (u1 - u0);
    if across <= 0.0 {
        return 0.0;
    }
    across * dash_coverage(v0, v1, 0.35, 0.6)
}

/// Fraction of the `[v0, v1)` span that falls on a painted dash. Both ends of the
/// row are considered, so a dash boundary inside a texel comes out as a partial
/// value rather than a hard step.
fn dash_coverage(v0: f32, v1: f32, on_m: f32, period_m: f32) -> f32 {
    let span = v1 - v0;
    if span <= 0.0 || period_m <= 0.0 {
        return 0.0;
    }
    // A texel is far shorter than a period in practice, so at most two dash
    // intervals can overlap it; walk them rather than integrating.
    let first = (v0 / period_m).floor() as i32;
    let last = (v1 / period_m).floor() as i32;
    let mut covered = 0.0;
    for k in first..=last {
        let start = k as f32 * period_m;
        covered += span_overlap(v0, v1, start, start + on_m);
    }
    (covered / span).clamp(0.0, 1.0)
}

/// Length of the overlap between `[a0, a1)` and `[b0, b1)`.
fn span_overlap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

/// Perturb a colour with two octaves of hash noise: per-texel grit, plus a
/// coarser blotch that survives a few mip levels and keeps a long straight road
/// from reading as a single flat band of grey.
fn shade(color: [u8; 3], amount: f32, x: u32, y: u32) -> [u8; 3] {
    if amount <= 0.0 {
        return color;
    }
    // The coarse octave uses `% TEX_*` on the block index so it wraps with the
    // texture; anything else would leave a visible seam every repeat.
    let fine = hash2(x, y) - 0.5;
    let coarse = hash2((x / 8) % (TEX_W / 8), (y / 8) % (TEX_H / 8) + 512) - 0.5;
    let n = 1.0 + amount * (fine * 0.6 + coarse * 1.4);
    [
        (f32::from(color[0]) * n).clamp(0.0, 255.0) as u8,
        (f32::from(color[1]) * n).clamp(0.0, 255.0) as u8,
        (f32::from(color[2]) * n).clamp(0.0, 255.0) as u8,
    ]
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (f32::from(a[0]) + (f32::from(b[0]) - f32::from(a[0])) * t) as u8,
        (f32::from(a[1]) + (f32::from(b[1]) - f32::from(a[1])) * t) as u8,
        (f32::from(a[2]) + (f32::from(b[2]) - f32::from(a[2])) * t) as u8,
    ]
}

/// Deterministic 2D value hash in `[0, 1)`. Cheap integer mixing — no `rand`
/// dependency, and the same texture every run (which matters: the surfaces are
/// regenerated per process, and a road that changed grain between a screenshot
/// pair would make `--shot` diffs useless).
fn hash2(x: u32, y: u32) -> f32 {
    let mut h = x
        .wrapping_mul(0x1656_67B1)
        .wrapping_add(y.wrapping_mul(0x27D4_EB2D));
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x2974_5C57);
    h ^= h >> 16;
    (h & 0xFF_FFFF) as f32 / 16_777_216.0
}

#[cfg(test)]
mod tests {
    /// **The track is standard gauge.**
    ///
    /// 1435 mm between the inner faces of the rails, which is the one railway
    /// dimension everybody can check against a photograph: the sleeper ends stick
    /// out past the rails by a fixed amount, so the *proportion* is what the eye
    /// reads, and a narrow gauge on a wide bed looks like a model railway.
    ///
    /// It was 1.28 m until 2026-09-19 — two round `u` values on a 3.2 m bed,
    /// which is how a number that is nobody's measurement survives: it is not
    /// wrong in a way that shows up as an error, only as a picture that is
    /// slightly off and that nobody has a reason to measure.
    #[test]
    fn the_track_is_standard_gauge() {
        let bed = road_width_m("rail", StreetFlags::default());
        let (_, _, markings) = markings_of(RoadClass::Rail);
        assert_eq!(markings.len(), 2, "a track has two rails");
        let centres_m = (markings[1].u - markings[0].u) * bed;
        let gauge_m = centres_m - RAIL_HEAD_M;
        assert!(
            (gauge_m - 1.435).abs() < 0.02,
            "gauge is {gauge_m} m on a {bed} m bed, not 1.435 m"
        );
    }

    /// **The hard shoulder is three metres wide on both fast classes.**
    ///
    /// A texture is built per class and has no width to divide by, so the
    /// `kantstreep` positions are ratios — and a ratio is only right for the
    /// width `road_width_m` actually hands out. This ties the two together: move
    /// the modelled width of a motorway and this fails rather than quietly
    /// painting a two-metre shoulder.
    ///
    /// Three metres is the Dutch standard (`vluchtstrook`), and it is what makes
    /// a motorway read as a motorway: a wide unmarked strip outside a continuous
    /// line. Until 2026-09-19 both classes had none at all — the outermost line
    /// sat at the edge of the tarmac, so the shoulder's width was painted as a
    /// running lane.
    #[test]
    fn the_hard_shoulder_is_three_metres() {
        let two_way = road_width_m("motorway", StreetFlags::default());
        let one_way = road_width_m(
            "motorway",
            StreetFlags {
                oneway: true,
                ..StreetFlags::default()
            },
        );
        assert!(
            (KANTSTREEP_U_22 * two_way - 3.0).abs() < 0.05,
            "{two_way} m"
        );
        assert!(
            (KANTSTREEP_U_12 * one_way - 3.0).abs() < 0.05,
            "{one_way} m"
        );
    }

    /// Nothing is painted in the hard shoulder, and no wheel runs there.
    ///
    /// The shoulder is the band outside the `kantstreep`. A lane dash out there
    /// would be inventing a lane nobody may drive in, and a polished wheel track
    /// would say traffic runs where it does not — both undo the thing the
    /// shoulder is *for*, which is reading as empty.
    #[test]
    fn the_shoulder_carries_no_lane_markings_or_wheel_tracks() {
        for (class, kantstreep) in [
            (RoadClass::Motorway, KANTSTREEP_U_22),
            (RoadClass::Carriageway, KANTSTREEP_U_12),
        ] {
            let outer = 1.0 - kantstreep;
            let (_, _, markings) = markings_of(class);
            for m in &markings {
                if m.dash.is_none() {
                    continue;
                }
                assert!(
                    m.u < outer && (class != RoadClass::Motorway || m.u > kantstreep),
                    "{class:?}: a lane dash at u={} lies in the hard shoulder",
                    m.u
                );
            }
            for w in class.wheel_paths() {
                assert!(
                    *w < outer && (class != RoadClass::Motorway || *w > kantstreep),
                    "{class:?}: a wheel track at u={w} runs in the hard shoulder"
                );
            }
        }
    }

    /// **A dash period that does not divide the texture's repeat stutters.**
    ///
    /// The texture is `texture_length_m` long and tiles along the road, so a dash
    /// pattern whose period does not fit a whole number of times is cut mid-dash
    /// at every wrap — a rhythm break at a fixed interval down the whole street.
    /// It is a tiling artifact that reads as a marking fault, and the only way to
    /// see it is to drive a long enough straight.
    ///
    /// The rule was written in `Marking::dash`'s doc and enforced by nothing,
    /// which is how `Major` came to be measured for the wrong standard: it tiled
    /// correctly at 3-and-5 over 16 m, so nothing objected.
    #[test]
    fn dash_period_tiles_the_texture() {
        for class in [
            RoadClass::Motorway,
            RoadClass::Carriageway,
            RoadClass::Major,
            RoadClass::Minor,
            RoadClass::Path,
            RoadClass::Rail,
        ] {
            let len = class.texture_length_m();
            let (_, _, markings) = markings_of(class);
            for m in &markings {
                let Some((on, period)) = m.dash else { continue };
                assert!(on < period, "{class:?}: a dash longer than its period");
                let repeats = len / period;
                assert!(
                    (repeats - repeats.round()).abs() < 1e-4,
                    "{class:?}: {period} m dash period does not divide a {len} m \
                     texture ({repeats} repeats) — the pattern jumps at every wrap"
                );
            }
        }
    }

    /// The lane dash is the Dutch standard: 3 m painted, 9 m gap.
    ///
    /// Pinned as a number rather than left to the constant's doc, because this is
    /// a fact about roads in the world and not a choice this code gets to make.
    /// `nl.wikipedia.org/wiki/Wegmarkering`.
    #[test]
    fn lane_dashes_are_three_metres_on_nine_off() {
        assert!((DASH_ON_M - 3.0).abs() < f32::EPSILON);
        assert!((DASH_PERIOD_M - DASH_ON_M - 9.0).abs() < f32::EPSILON);
    }

    use super::*;

    const CLASSES: [RoadClass; 6] = [
        RoadClass::Motorway,
        RoadClass::Carriageway,
        RoadClass::Major,
        RoadClass::Minor,
        RoadClass::Path,
        RoadClass::Rail,
    ];

    /// The surface map must be the same size and shape as the colour, or the
    /// mip chain built from it lands on a different grid and the two sample out
    /// of registration.
    #[test]
    fn the_surface_map_matches_the_colour_map() {
        for class in CLASSES {
            let t = road_texture(class);
            assert_eq!(t.surface.len(), t.rgba.len(), "{class:?}");
            assert_eq!(
                t.surface.len(),
                (t.width * t.height * 4) as usize,
                "{class:?}"
            );
        }
    }

    /// Nothing may reach a perfect mirror — see `MIN_ROUGHNESS`, and note the
    /// scalar in the material lowers this again in the rain.
    #[test]
    fn no_texel_is_a_mirror() {
        for class in CLASSES {
            let t = road_texture(class);
            let floor = to_u8(MIN_ROUGHNESS);
            for texel in t.surface.chunks_exact(4) {
                assert!(texel[1] >= floor, "{class:?} roughness {}", texel[1]);
            }
        }
    }

    /// Only a railway is metal. A metallic road surface would take its colour
    /// from the environment map instead of from its own tarmac, which on a
    /// software renderer with no environment map is simply black.
    #[test]
    fn only_the_rails_are_metallic() {
        for class in CLASSES {
            let t = road_texture(class);
            let metal = t.surface.chunks_exact(4).any(|texel| texel[2] > 8);
            assert_eq!(metal, class == RoadClass::Rail, "{class:?}");
        }
    }

    /// The wheel paths must actually be smoother than the tarmac beside them,
    /// which is the whole of what this map buys on an ordinary street. Compared
    /// against the *gutter*, which no wheel runs in on any class.
    #[test]
    fn traffic_polishes_the_wheel_paths() {
        let t = road_texture(RoadClass::Minor);
        let row = (TEX_H / 2) as usize;
        let at = |u: f32| {
            let x = (u * TEX_W as f32) as usize;
            t.surface[(row * TEX_W as usize + x) * 4 + 1]
        };
        // 0.24 is a wheel path for `Minor`; 0.5 is the crown between them.
        assert!(
            at(0.24) < at(0.5),
            "wheel path {} should be smoother than the crown {}",
            at(0.24),
            at(0.5)
        );
    }

    #[test]
    fn every_class_generates_a_well_formed_opaque_texture() {
        for class in CLASSES {
            let t = road_texture(class);
            assert_eq!(t.width, TEX_W);
            assert_eq!(t.height, TEX_H);
            assert_eq!(t.rgba.len() as u32, TEX_W * TEX_H * 4);
            assert!(
                t.rgba.chunks_exact(4).all(|p| p[3] == 0xFF),
                "{class:?} surface must be opaque"
            );
        }
    }

    /// The whole point of a centre line is that it is at the centre. Sample the
    /// middle column against a quarter-width one and require it to be brighter
    /// somewhere along the road (a dash is on for part of its period).
    #[test]
    fn marked_classes_have_a_brighter_centre_line() {
        for class in [RoadClass::Motorway, RoadClass::Major] {
            let t = road_texture(class);
            let lum = |x: u32, y: u32| -> u32 {
                let i = ((y * t.width + x) * 4) as usize;
                u32::from(t.rgba[i]) + u32::from(t.rgba[i + 1]) + u32::from(t.rgba[i + 2])
            };
            let centre = (0..t.height).map(|y| lum(t.width / 2, y)).max().unwrap();
            let lane = (0..t.height).map(|y| lum(t.width / 4, y)).max().unwrap();
            assert!(
                centre > lane,
                "{class:?}: centre {centre} not brighter than lane interior {lane}"
            );
        }
    }

    /// A dashed line must actually be intermittent, or the texture is drawing a
    /// solid line where the cartography says dashes.
    #[test]
    fn dashes_alternate_along_the_road() {
        let t = road_texture(RoadClass::Major);
        let lum = |y: u32| -> u32 {
            let i = ((y * t.width + t.width / 2) * 4) as usize;
            u32::from(t.rgba[i]) + u32::from(t.rgba[i + 1]) + u32::from(t.rgba[i + 2])
        };
        let (lo, hi) = (0..t.height).fold((u32::MAX, 0), |(lo, hi), y| {
            (lo.min(lum(y)), hi.max(lum(y)))
        });
        assert!(
            hi > lo + 200,
            "centre line looks continuous (lo {lo}, hi {hi})"
        );
    }

    #[test]
    fn dash_coverage_is_a_fraction_that_matches_the_duty_cycle() {
        // A row spanning exactly one full period sees the whole duty cycle.
        let c = dash_coverage(0.0, 8.0, 3.0, 8.0);
        assert!((c - 3.0 / 8.0).abs() < 1e-5, "got {c}");
        // Fully inside a painted run, and fully inside a gap.
        assert!((dash_coverage(0.5, 1.0, 3.0, 8.0) - 1.0).abs() < 1e-5);
        assert_eq!(dash_coverage(4.0, 5.0, 3.0, 8.0), 0.0);
    }

    #[test]
    fn subways_and_unknown_kinds_are_not_drawn() {
        let both = StreetFlags::default();
        assert_eq!(road_class("subway", both), None);
        assert_eq!(road_class("elevator", both), None);
        assert_eq!(road_class("motorway", both), Some(RoadClass::Motorway));
        assert_eq!(road_class("residential", both), Some(RoadClass::Minor));
    }

    /// A dual carriageway arrives as two one-way features a median apart. Drawing
    /// each at the full two-way width is what laid two slabs of tarmac over one
    /// another; each half must come out roughly half as wide, and without the
    /// median markings that belong to the gap between them.
    #[test]
    fn a_one_way_fast_road_is_half_a_carriageway() {
        let oneway = StreetFlags {
            oneway: true,
            ..StreetFlags::default()
        };
        let both = StreetFlags::default();
        for kind in ["motorway", "trunk", "primary", "secondary"] {
            let (w1, w2) = (road_width_m(kind, oneway), road_width_m(kind, both));
            assert!(
                w1 < w2 && w1 > w2 * 0.45,
                "{kind}: one-way {w1} m against two-way {w2} m"
            );
            assert_eq!(road_class(kind, oneway), Some(RoadClass::Carriageway));
        }
        // Below the fast classes `oneway` is about the direction of travel, not
        // about a second carriageway somewhere else — the width must not move.
        for kind in ["residential", "service", "cycleway"] {
            assert_eq!(road_width_m(kind, oneway), road_width_m(kind, both));
            assert_ne!(road_class(kind, oneway), Some(RoadClass::Carriageway));
        }
    }

    /// A slip road is a lane or two wide whatever it ramps off — drawn at the
    /// motorway's own width it is most of what makes an interchange a blob.
    #[test]
    fn a_ramp_is_narrower_than_the_road_it_leaves() {
        let link = StreetFlags {
            oneway: true,
            link: true,
            ..StreetFlags::default()
        };
        for kind in ["motorway", "trunk", "primary"] {
            assert!(
                road_width_m(kind, link) < road_width_m(kind, StreetFlags::default()) * 0.6,
                "{kind}_link is not narrow"
            );
            assert_eq!(road_class(kind, link), Some(RoadClass::Carriageway));
        }
    }

    /// The whole point of the class: no median, so the two halves of a dual
    /// carriageway do not each paint the line that belongs between them.
    #[test]
    fn a_carriageway_has_no_centre_line() {
        let t = road_texture(RoadClass::Carriageway);
        let lum = |x: u32, y: u32| -> u32 {
            let i = ((y * t.width + x) * 4) as usize;
            u32::from(t.rgba[i]) + u32::from(t.rgba[i + 1]) + u32::from(t.rgba[i + 2])
        };
        let centre = (0..t.height).map(|y| lum(t.width / 2, y)).max().unwrap();
        let shoulder = (0..t.height)
            .flat_map(|y| (0..5).map(move |x| (x, y)))
            .map(|(x, y)| lum(x, y))
            .max()
            .unwrap();
        assert!(
            centre < shoulder,
            "centre {centre} is painted like the shoulder line {shoulder}"
        );
    }

    /// Eyeball hatch, like `vector_tiles::dump_png`: writes each class's surface
    /// so the markings can be looked at.
    /// `cargo test -p cartopolis_geo dump_road_textures -- --ignored`
    #[test]
    #[ignore = "eyeball hatch: it writes PNGs to look at, it asserts nothing"]
    fn dump_road_textures() {
        for class in CLASSES {
            let t = road_texture(class);
            let name = format!("/tmp/road_{class:?}.png").to_lowercase();
            image::save_buffer(
                &name,
                &t.rgba,
                t.width,
                t.height,
                image::ExtendedColorType::Rgba8,
            )
            .expect("write png");
            println!("wrote {name}");
        }
    }
}
