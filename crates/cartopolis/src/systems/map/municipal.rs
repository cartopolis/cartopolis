//! **Municipal registers**: the street objects a *city* surveys, where the
//! national ones are silent.
//!
//! `systems::surveyed` reads the BGT, which is national and is the right default
//! precisely because it is. What it is not is complete: its point collections are
//! *optional* content, filled per municipality, and the hole they leave is not at
//! the edges but in the middle. Over the cell that covers the study quay in
//! Groningen the BGT registers **no trees at all**; the city's own register has
//! 3,018. Two cells out, where the BGT does answer, it answers with 112 and 230
//! against the city's 5,014 and 4,415. Street lighting is the same hole again:
//! the BGT's pole register is empty over central Groningen, and the city's own
//! carries 493 columns in one cell of it.
//!
//! # A register produces shapes the client already has
//!
//! The hard rule, and what stops this growing a vocabulary of its own. The client
//! can already stand four kinds of thing on the ground — a tree with a height, a
//! prop with a [`FurnitureKind`], a paved polygon, a boundary line — and each has
//! a builder that does the work. A register's parser normalises into one of those
//! and nothing else; anything the city knows that does not fit is dropped **at
//! the parser**, where the city's own vocabulary is still understood, rather than
//! carried inward as a field only one city fills.
//!
//! [`RegisteredCell`] therefore mirrors `surveyed::SurveyedCell`: one vector per
//! shape, not one enum with variants. That is not a style choice — the consumers
//! genuinely differ. Trees are merged into the canopy meshes; props go through
//! `furniture::place_surveyed`, which faces each one at the nearest street, so a
//! city's lamp post is turned to the kerb by the code that turns a bench.
//!
//! # One register per city, several sources per register
//!
//! There is no national municipal API and there is not going to be one. Every
//! city publishes on whatever it already runs, in its own coordinate system, with
//! its own field names and its own vocabulary. So a [`Register`] owns the whole
//! question for one city, and each of its [`Source`]s is one of that city's
//! layers plus the two or three lines that read its attributes into ours.
//!
//! The split earns itself on the first city: Groningen's trees, lamp posts,
//! traffic lights and bus stops are four layers, and the last three have
//! byte-identical schemas — so they share [`parse_geoserver_rd`] and differ only
//! in a `layer` string and a closure. Adding Utrecht is a [`Register`] with its
//! own sources; no caller changes, because no caller ever sees a city's wire
//! format.
//!
//! # A city register replaces the national one, per kind
//!
//! This is the opposite of the rule `systems::surveyed` follows against the
//! *scatter*, and for a reason that does not apply there. The scatter fills park
//! and wood polygons while a surveyed tree stands on a pavement, so the two
//! describe different trees and adding them is right. Two **registers** describe
//! the same objects: the BGT's 112 trees in a cell are a subset of the city's
//! 5,014, so drawing both plants a second lime a metre from the first and the
//! pair z-fight their own canopies. Matching them up by distance would be
//! arbitration — a tolerance to tune, wrong in both directions — so the overlap
//! is removed instead.
//!
//! It is removed **per kind, not per cell**, which is what [`Source::supplies`]
//! is for. A city that registers trees and lamp posts has nothing to say about
//! benches, so suppressing the whole BGT cell over its ground would delete the
//! benches in order to fix the trees.
//!
//! # What it costs, and what the cost actually is (measured 2026-09-07)
//!
//! Less than it looks. The densest Groningen tree cell is 1,223 trees and 324 kB
//! of GeoJSON — but the server gzips and `ureq` asks for it, so the **wire** cost
//! is 27 kB, and the normalised cache entry is ~50 bytes a tree. A whole session
//! over Groningen fetched 14 cells for 32,398 trees.
//!
//! So the cost worth designing against is **latency, not bytes**: every source is
//! one more request standing between a cell's ground and its mesh job. They are
//! fetched together rather than one after another for that reason. Worth naming
//! that natively this is presentation rather than overlap — `http::fetch` is a
//! blocking `ureq` call with no await point in it, exactly as `map_geometry`
//! records for the two optional layers it already zips — while on wasm it is a
//! real overlap. On both it is what stops a second source doubling the wait.
//!
//! # The cache invalidates itself
//!
//! Nothing here revalidates a stored cell: the sweep is by size, not by age. So a
//! reader that changes and a key that does not is a permanent wrong answer, and
//! this codebase has shipped exactly that before — `surveyed::cell_cache_key`
//! carries a hand-bumped `v2` because a fix to the *extractor* could not reach
//! cells already written under `v1`.
//!
//! A constant somebody has to remember to bump is the same bug waiting, so the
//! key carries a **fingerprint of the reader's own behaviour** instead
//! ([`source_fingerprint`]): each source's reader is run over an embedded fixture
//! page at startup and the normalised result is hashed. Change the layer name,
//! the attribute, the height bands, or which lamp types are refused, and the
//! fingerprint moves, the key moves, and the old entries are simply never read
//! again. No discipline required and none relied on.
//!
//! Its one limit is worth stating plainly: **the fixture is the contract.** A
//! reader change on an input the fixture does not contain will not move the
//! fingerprint. That is what [`Register::fingerprint_page`] is sized for — it carries every
//! branch each source actually decides on — and what
//! `the_fingerprint_moves_when_a_reader_changes` pins.
//!
//! [`CACHE_GENERATION`] is the second axis and stays a constant, because it is
//! for the thing a fingerprint cannot see: a change to the stored *format*
//! itself, which invalidates every source of every city at once.

use crate::platform::{http, storage};
use crate::systems::furniture::{FurnitureInstance, FurnitureKind};
use crate::systems::height::{rd_to_wgs84, wgs84_to_rd};
use crate::systems::scatter::TreeInstance;
use cartopolis_core::world::tile_to_lat_lng;

/// The zoom cells are cut at — `map_geometry::GEOMETRY_ZOOM`, because these
/// objects are baked into that cell's merged meshes and have to arrive on the
/// worker that builds them.
const REGISTER_ZOOM: u32 = 14;

/// The stored *format*'s generation — see the module note. Bump it when
/// [`write_normalised`] changes shape, and only then; a reader change moves its
/// own fingerprint without touching this.
const CACHE_GENERATION: &str = "v2";

/// Most objects one cell may take from one source.
///
/// **Deliberately not `scatter::MAX_TREES_PER_CELL`**, and the two caps bound
/// different things. The scatter's bounds a *generated* lattice — a cell that is
/// all forest would emit ~35,000 trees and the number is a decision, not a fact.
/// This one bounds a **count of real objects**, so any cell it actually cuts is a
/// cell drawn wrong. Reusing 6,000 did exactly that on the first run: two
/// Groningen cells were truncated at 6,000 out of 6,263 and 6,808, quietly losing
/// 800 surveyed trees.
///
/// 12,000 is ~20× the municipal mean (126,060 trees over 447 km², against
/// 2.14 km² in a z14 cell here) and ~1.8× the densest cell measured. It is a
/// backstop against a register that answers with something absurd, not a sampling
/// rule — which is why hitting it is a logged warning.
const MAX_PER_SOURCE_PER_CELL: usize = 12_000;

/// One tree as its municipality registered it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegisteredTree {
    pub lat: f64,
    pub lng: f64,
    /// Height of the crown above the ground in metres, when the register says.
    ///
    /// `None` is ordinary, not an error — Groningen leaves it blank on about one
    /// tree in thirty — and means "draw it the way an unmeasured street tree has
    /// always been drawn", so a register with no heights at all still works and
    /// looks exactly like the BGT's trees do today.
    pub height_m: Option<f32>,
}

/// One street object as its municipality registered it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegisteredProp {
    pub lat: f64,
    pub lng: f64,
    pub kind: FurnitureKind,
    /// sRGB body colour, where the register says something the form can show.
    ///
    /// `None` means "the colour this form has always been"
    /// (`furniture::default_paint`), which is what every object without a
    /// colour vocabulary of its own gets. The one that has one today is the
    /// bottle bank: its fraction is the whole point of the object, and a green
    /// one and a blue one are the same six boxes in two colours.
    pub paint: Option<u32>,
}

/// What a source's reader decides about one feature: which form it takes, and
/// what colour if the register said.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub kind: FurnitureKind,
    pub paint: Option<u32>,
}

impl Placed {
    /// The common case: a form the register names and no colour with it.
    #[must_use]
    pub const fn plain(kind: FurnitureKind) -> Self {
        Self { kind, paint: None }
    }

    #[must_use]
    pub const fn painted(kind: FurnitureKind, paint: u32) -> Self {
        Self {
            kind,
            paint: Some(paint),
        }
    }
}

/// One cell's registered objects, still in lat/lng, one vector per shape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegisteredCell {
    pub trees: Vec<RegisteredTree>,
    pub props: Vec<RegisteredProp>,
}

impl RegisteredCell {
    fn absorb(&mut self, other: RegisteredCell) {
        self.trees.extend(other.trees);
        self.props.extend(other.props);
    }

    fn is_empty(&self) -> bool {
        self.trees.is_empty() && self.props.is_empty()
    }
}

/// What one of a city's layers contributes, and how to read the city's own
/// attributes into the client's vocabulary.
///
/// The closure may refuse. Groningen's lighting layer is 481 lamp posts, 8 ground
/// spots, 3 tunnel lights and a clock over one cell; a ground spot drawn as a
/// four-metre column is worse than not drawing it, so [`Yields::Props`] answers
/// `None` for those.
pub enum Yields {
    Trees { height: fn(Fields) -> Option<f32> },
    Props { kind: fn(Fields) -> Option<Placed> },
}

/// One layer of one city's register.
pub struct Source {
    /// Cache-key segment and log name. Stable — renaming it orphans this
    /// source's stored cells rather than serving them to the wrong reader.
    pub id: &'static str,
    /// The city's own layer name, as its service spells it.
    pub layer: &'static str,
    /// The one attribute the reader below looks at, so the request can ask for it
    /// and nothing else. The tree layer carries fifteen fields, and the full
    /// record is 615 bytes a tree against 265 for the geometry plus one.
    pub attribute: &'static str,
    pub yields: Yields,
    /// The national-register content this source stands in for, so the BGT's own
    /// copy of it can be suppressed over this city's ground.
    ///
    /// Declared rather than derived: [`Yields::Props`] is a closure and there is
    /// no way to ask it what it might return. Declaring it is also the honest
    /// place for the claim — "this source is the authority on lamp posts here" is
    /// a statement about the data, not about the code.
    pub supplies: &'static [Supply],
}

/// A kind of content a source is the authority on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Supply {
    Trees,
    Prop(FurnitureKind),
}

/// One municipality's register: where it applies, and every layer of it we read.
pub struct Register {
    /// Short, stable, and part of the cache key.
    pub id: &'static str,
    /// The attribution line. Not decoration: these are published under CC BY,
    /// unlike the CC0 national registers around them, so the credit is a licence
    /// condition and the layer may not ship without showing it.
    pub attribution: &'static str,
    /// Coverage as `(south, west, north, east)` in WGS84 degrees.
    ///
    /// A box rather than the municipal boundary polygon, and deliberately: the
    /// cost of being too generous is one request that answers with nothing, and
    /// the cost of carrying 342 municipal boundaries to be exact is a dependency
    /// on a second dataset that also has to be kept current.
    pub bounds: (f64, f64, f64, f64),
    /// This city's endpoint, given one of its sources and a cell.
    pub url: fn(&Source, cx: i64, cy: i64) -> String,
    /// This city's answer, normalised.
    pub parse: fn(&str, &Yields) -> Result<RegisteredCell, String>,
    /// A page in this city's own wire format, carrying every branch its sources
    /// decide on. Never fetched — it is the input the cache fingerprint is taken
    /// over. See the module note on what that does and does not catch.
    pub fingerprint_page: &'static str,
    pub sources: &'static [Source],
}

/// Every city whose register this build can read.
pub static REGISTERS: &[Register] = &[GRONINGEN];

/// The bisecting switch, read once. One `std::env::var` per cell per source
/// would be a syscall on the mesh path for a value that cannot change.
static DISABLED: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var("CARTO_NO_MUNICIPAL").is_ok());

/// Which register covers a cell, if any — the cell's **centre**, so a cell
/// straddling a city boundary belongs to exactly one register and cannot be read
/// twice.
#[must_use]
pub fn register_for(cx: i64, cy: i64) -> Option<&'static Register> {
    if *DISABLED {
        return None;
    }
    let (north, west) = tile_to_lat_lng(cx, cy, REGISTER_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, REGISTER_ZOOM);
    let (lat, lng) = ((north + south) / 2.0, (west + east) / 2.0);
    REGISTERS.iter().find(|r| {
        (r.bounds.0..=r.bounds.2).contains(&lat) && (r.bounds.1..=r.bounds.3).contains(&lng)
    })
}

/// Does a city register stand in for this content over this cell?
///
/// The question `map_geometry` asks of the BGT before reading it — per kind, for
/// the reason the module note gives.
#[must_use]
pub fn supersedes(cx: i64, cy: i64, supply: Supply) -> bool {
    register_for(cx, cy).is_some_and(|r| r.sources.iter().any(|s| s.supplies.contains(&supply)))
}

/// Every prop kind a city register stands in for over this cell.
///
/// Answered once per cell rather than per object: the caller filters a list of
/// placed furniture against it, and asking [`supersedes`] per item would redo the
/// coverage lookup a few hundred times a cell.
#[must_use]
pub fn supplied_props(cx: i64, cy: i64) -> Vec<FurnitureKind> {
    register_for(cx, cy)
        .map(|r| {
            r.sources
                .iter()
                .flat_map(|s| s.supplies)
                .filter_map(|s| match s {
                    Supply::Prop(kind) => Some(*kind),
                    Supply::Trees => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

// ─── The self-invalidating cache key ─────────────────────────────────────────

/// FNV-1a, 64-bit, folded to 32.
///
/// Hand-rolled and not `DefaultHasher` on purpose: this number goes into a cache
/// key that has to mean the same thing on every platform and every build, and
/// std makes no stability promise about its hashers across releases.
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    ((h >> 32) as u32) ^ (h as u32)
}

/// What this source's reader *does*, as one number: the layer and attribute it
/// asks for, plus the normalised result of running it over the register's fixture
/// page. See the module note.
fn source_fingerprint(register: &Register, source: &Source) -> u32 {
    let parsed = (register.parse)(register.fingerprint_page, &source.yields)
        .map(|cell| write_normalised(&cell))
        // A fixture that no longer parses is itself a change worth invalidating
        // on, and it is caught at test time by `every_source_reads_the_fixture`.
        .unwrap_or_else(|e| format!("unreadable:{e}"));
    fnv1a(format!("{}\u{1}{}\u{1}{parsed}", source.layer, source.attribute).as_bytes())
}

/// Fingerprints, computed once. Keyed by the two ids that already have to be
/// unique for the cache key to work at all.
static FINGERPRINTS: std::sync::LazyLock<
    std::collections::HashMap<(&'static str, &'static str), u32>,
> = std::sync::LazyLock::new(|| {
    let map: std::collections::HashMap<_, _> = REGISTERS
        .iter()
        .flat_map(|r| {
            r.sources
                .iter()
                .map(move |s| ((r.id, s.id), source_fingerprint(r, s)))
        })
        .collect();
    for ((register, source), fp) in &map {
        // Logged because "why did every cell re-download after that release?" is
        // otherwise unanswerable from a log, and the answer is always this number.
        tracing::info!(
            register,
            source,
            fingerprint = format!("{fp:08x}"),
            "municipal register reader fingerprint"
        );
    }
    map
});

fn cache_key(register: &Register, source: &Source, cx: i64, cy: i64) -> String {
    let fp = FINGERPRINTS
        .get(&(register.id, source.id))
        .copied()
        .unwrap_or_default();
    format!(
        "municipal/{}/{}/{CACHE_GENERATION}-{fp:08x}/{REGISTER_ZOOM}/{cx}/{cy}.json",
        register.id, source.id
    )
}

// ─── Fetch ───────────────────────────────────────────────────────────────────

/// Fetch and parse one cell from every source of whichever register covers it.
///
/// Empty outside every register's coverage, and empty when a city's service is
/// unreachable — in both cases the caller's fallback is the layer it already had.
///
/// The cache stores the normalised form, **one entry per source**: per source and
/// not per cell so that adding a layer does not invalidate the trees already
/// stored, and normalised rather than the city's own answer because every field
/// that answer carries beyond these was already discarded by the parser that
/// wrote it.
pub(crate) async fn load_registered_cell(cx: i64, cy: i64) -> RegisteredCell {
    let Some(register) = register_for(cx, cy) else {
        return RegisteredCell::default();
    };
    // Together rather than one after another: each source is a request standing
    // between this cell's ground and its mesh job, so serially a second source
    // doubles the wait. The module note says what this buys on each platform.
    let parts = futures_util::future::join_all(
        register
            .sources
            .iter()
            .map(|source| load_source(register, source, cx, cy)),
    )
    .await;

    let mut cell = RegisteredCell::default();
    for part in parts {
        cell.absorb(part);
    }
    if !cell.is_empty() {
        mark_used(register.id);
    }
    cell
}

async fn load_source(
    register: &'static Register,
    source: &'static Source,
    cx: i64,
    cy: i64,
) -> RegisteredCell {
    let key = cache_key(register, source, cx, cy);
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        return parse_normalised(&cached).unwrap_or_default();
    }
    if REGISTER_BREAKER.is_open_at(web_time::Instant::now()) {
        return RegisteredCell::default();
    }
    // The measured worst case is 0.73 s for the densest Groningen cell
    // (2026-09-05, three runs), so the shared optional-layer bound is four times
    // the slowest answer this layer has ever given — and the point of it is the
    // *unresponsive* case, where the ground must not wait on a city.
    let request =
        http::Request::get((register.url)(source, cx, cy)).timeout(http::OPTIONAL_LAYER_TIMEOUT);
    let Ok(bytes) = http::fetch(request).await else {
        REGISTER_BREAKER.trip();
        return RegisteredCell::default();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        REGISTER_BREAKER.trip();
        return RegisteredCell::default();
    };
    let cell = match (register.parse)(&text, &source.yields) {
        Ok(cell) => cell,
        Err(e) => {
            tracing::warn!(register = register.id, source = source.id, cell = ?(cx, cy),
                error = %e, "municipal register did not parse");
            REGISTER_BREAKER.trip();
            return RegisteredCell::default();
        }
    };
    storage::put(
        storage::Namespace::Cache,
        &key,
        write_normalised(&cell).as_bytes(),
    )
    .await;
    cell
}

/// One outage gate for the whole layer rather than one per city.
///
/// The other third-party layers here keep a breaker per service because theirs
/// can genuinely fail apart. These cannot overlap: a cell belongs to exactly one
/// register, so no two are ever tried for the same ground. Split it the day two
/// registers are reachable for one cell, which is the day the coverage boxes are
/// allowed to overlap.
pub(crate) static REGISTER_BREAKER: http::Breaker = http::Breaker::new();

/// Registers that have answered with something. See [`attributions`].
static USED: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

fn mark_used(id: &'static str) {
    if let Ok(mut u) = USED.lock() {
        if !u.contains(&id) {
            u.push(id);
        }
    }
}

/// The attribution lines for every register that has actually put something on
/// the map this session.
///
/// Gated on use rather than listed statically, because a credit for a city the
/// player has never been near is noise in a panel that is already a wall of
/// them — and because the licence obliges attribution for data *used*.
#[must_use]
pub fn attributions() -> Vec<&'static str> {
    let used = USED.lock().map(|u| u.clone()).unwrap_or_default();
    REGISTERS
        .iter()
        .filter(|r| used.iter().any(|id| *id == r.id))
        .map(|r| r.attribution)
        .collect()
}

// ─── Into the client's own builders ──────────────────────────────────────────

/// This cell's registered trees as instances, ready to join the scattered ones.
///
/// Objects outside the cell are dropped here rather than in the request: a
/// bounding box in the city's coordinate system does not have the cell's edges in
/// it (the two grids are rotated against each other), so the request is
/// deliberately a little generous and containment is settled in the cell's frame.
#[must_use]
pub fn registered_trees(
    cell: &RegisteredCell,
    cx: i64,
    cy: i64,
    tile_m: f32,
    seed: u64,
) -> Vec<TreeInstance> {
    let points: Vec<(f32, f32, Option<f32>)> = cell
        .trees
        .iter()
        .filter_map(|t| {
            crate::systems::surveyed::cell_local(t.lat, t.lng, cx, cy, REGISTER_ZOOM, tile_m)
                .map(|(x, z)| (x, z, t.height_m))
        })
        .collect();
    crate::systems::scatter::surveyed_trees(&points, seed)
}

/// This cell's registered street objects, oriented against the tile's own
/// streets — the same builder the BGT's props go through.
#[must_use]
pub fn registered_props(
    cell: &RegisteredCell,
    tile: &crate::systems::vector_tiles::TileData,
    cx: i64,
    cy: i64,
    tile_m: f32,
    seed: u64,
) -> Vec<FurnitureInstance> {
    let points: Vec<(f32, f32, FurnitureKind, Option<u32>)> = cell
        .props
        .iter()
        .filter_map(|p| {
            crate::systems::surveyed::cell_local(p.lat, p.lng, cx, cy, REGISTER_ZOOM, tile_m)
                .map(|(x, z)| (x, z, p.kind, p.paint))
        })
        .collect();
    crate::systems::furniture::place_surveyed(tile, tile_m, seed, &points)
}

// ─── Groningen ───────────────────────────────────────────────────────────────

/// Gemeente Groningen's registers, over its own GeoServer.
///
/// Bounds are the tree layer's published extent (RD 227,074–246,669 E by
/// 569,831–592,673 N) converted to degrees, so they are the municipality's reach
/// and not a guess at it.
const GRONINGEN: Register = Register {
    id: "groningen",
    attribution: "Street objects: gemeente Groningen open data (CC BY 4.0)",
    bounds: (53.1074, 6.4635, 53.3156, 6.7626),
    url: groningen_url,
    parse: parse_geoserver_rd,
    fingerprint_page: GRONINGEN_FIXTURE,
    sources: &[
        Source {
            id: "trees",
            layer: "geo-data:Bomen gemeente Groningen",
            attribute: "BOOMHOOGTE",
            yields: Yields::Trees {
                height: |f| f.str("BOOMHOOGTE").and_then(height_band_m),
            },
            supplies: &[Supply::Trees],
        },
        Source {
            id: "lamps",
            layer: "geo-data:Lichtmasten gemeente Groningen",
            attribute: "LICHTPUNT_SOORT",
            yields: Yields::Props {
                // `LICHTPUNT_SOORT` over one cell: 481 `Lichtmast`, 8
                // `Grondspot`, 3 `Tunnelverlichting`, 1 `Klok`. Only the column
                // has a form here; a ground spot drawn as one would be a
                // four-metre post standing where there is a light in the paving.
                kind: |f| match f.str("LICHTPUNT_SOORT") {
                    Some("Lichtmast") => Some(Placed::plain(FurnitureKind::Lamp)),
                    _ => None,
                },
            },
            supplies: &[Supply::Prop(FurnitureKind::Lamp)],
        },
        Source {
            id: "containers",
            layer: "geo-data:CONTAINERS",
            attribute: "FRACTIE",
            yields: Yields::Props {
                // The fraction is not a new *form* — a bottle bank and a paper
                // bank are the same six boxes — so it is a colour, which is what
                // `FurnitureInstance::paint` already exists for. Four kinds would
                // have meant four meshes and four table entries.
                kind: |f| {
                    Some(Placed::painted(
                        FurnitureKind::Recycling,
                        fraction_colour(f.str("FRACTIE")?)?,
                    ))
                },
            },
            supplies: &[Supply::Prop(FurnitureKind::Recycling)],
        },
        Source {
            id: "fountains",
            layer: "geo-data:Fonteinen_gemeente_Groningen",
            attribute: "SOORT_KUNSTWERK",
            yields: Yields::Props {
                // The layer is `Fontein` on every feature seen; the match is
                // still on the value rather than a blanket accept, because the
                // collection is the city's *kunstwerken* register and a
                // different `SOORT` appearing in it later is a thing we have no
                // form for, not a fountain.
                kind: |f| match f.str("SOORT_KUNSTWERK") {
                    Some("Fontein") => Some(Placed::plain(FurnitureKind::Fountain)),
                    _ => None,
                },
            },
            supplies: &[Supply::Prop(FurnitureKind::Fountain)],
        },
        Source {
            id: "chargers",
            layer: "geo-data:Laadpalen_gemeente_Groningen",
            attribute: "STATUS_LAADPAAL",
            yields: Yields::Props {
                // A charge point is a box on a plinth, which is `Cabinet`.
                //
                // `STATUS_LAADPAAL` is `Bestaande laadpaal` on all 611 in the
                // city and nothing else, so this accepts that one value and
                // refuses the rest — the *safe* direction for a register with a
                // one-word vocabulary today. A city that later files planned or
                // withdrawn units in the same layer would otherwise have them
                // drawn as posts standing where there is nothing, and a missing
                // charge point is a smaller wrong than an invented one.
                kind: |f| match f.str("STATUS_LAADPAAL") {
                    Some("Bestaande laadpaal") => Some(Placed::plain(FurnitureKind::Cabinet)),
                    _ => None,
                },
            },
            supplies: &[Supply::Prop(FurnitureKind::Cabinet)],
        },
    ],
};

/// What a Dutch waste fraction is painted, in sRGB.
///
/// The city's own vocabulary, not the app's palette — the same footing
/// `nav_marks` paints its buoys on, which take a register's colours rather than
/// the furniture table's. `RESTAFVAL` is the painted steel a bottle bank was
/// already drawn in, so the commonest container by far (46 of 63 in one cell) is
/// unchanged; the three sorted fractions take the colours the lids actually are
/// here.
fn fraction_colour(fraction: &str) -> Option<u32> {
    Some(match fraction {
        "RESTAFVAL" => 0x9A_98_93,
        "PAPIER" => 0x3E_5E_8C,
        "GLAS" => 0x3F_6B_4A,
        "TEXTIEL" => 0xB5_6A_36,
        // An unknown fraction is still a container and is still worth drawing —
        // in the default steel, which says only "a bottle bank", which is true.
        _ => return Some(0x9A_98_93),
    })
}

/// The page every Groningen reader is fingerprinted over. Carries both sources'
/// attributes and every branch either of them decides on: a closed height band,
/// an open one at each end, an absent one, a lamp column, and two lighting kinds
/// that are refused.
const GRONINGEN_FIXTURE: &str = r#"{"type":"FeatureCollection","features":[
  {"geometry":{"type":"Point","coordinates":[234729.772,582447.843]},
   "properties":{"BOOMHOOGTE":"15 tot 18 m.","LICHTPUNT_SOORT":"Lichtmast",
                 "FRACTIE":"RESTAFVAL","SOORT_KUNSTWERK":"Fontein",
                 "STATUS_LAADPAAL":"Bestaande laadpaal"}},
  {"geometry":{"type":"Point","coordinates":[234800.0,582500.0]},
   "properties":{"BOOMHOOGTE":"tot 6 m.","LICHTPUNT_SOORT":"Grondspot",
                 "FRACTIE":"PAPIER","SOORT_KUNSTWERK":"Brug",
                 "STATUS_LAADPAAL":"Bestaande laadpaal"}},
  {"geometry":{"type":"Point","coordinates":[234850.0,582550.0]},
   "properties":{"BOOMHOOGTE":"24 m. en hoger","LICHTPUNT_SOORT":"Klok",
                 "FRACTIE":"GLAS","SOORT_KUNSTWERK":null,
                 "STATUS_LAADPAAL":"Ingetrokken"}},
  {"geometry":{"type":"Point","coordinates":[234900.0,582600.0]},
   "properties":{"BOOMHOOGTE":null,"LICHTPUNT_SOORT":"Lichtmast",
                 "FRACTIE":"TEXTIEL","SOORT_KUNSTWERK":"Fontein",
                 "STATUS_LAADPAAL":null}}]}"#;

/// One cell of one Groningen layer, as GeoJSON in RD metres.
///
/// Two query parameters are load-bearing. `propertyName` asks for the geometry
/// and the source's one attribute and nothing else. And `count` is
/// [`MAX_PER_SOURCE_PER_CELL`], so one pathological cell cannot hand the streamer
/// an unbounded download; a cell that hits it is logged rather than silently
/// truncated.
fn groningen_url(source: &Source, cx: i64, cy: i64) -> String {
    let [x0, y0, x1, y1] = cell_rd_bbox(cx, cy);
    format!(
        "https://maps.groningen.nl/geoserver/wfs\
         ?service=WFS&version=2.0.0&request=GetFeature\
         &typeNames={layer}&outputFormat=application%2Fjson\
         &propertyName=GEOM%2C{attribute}&count={MAX_PER_SOURCE_PER_CELL}\
         &bbox={x0:.1},{y0:.1},{x1:.1},{y1:.1},EPSG:28992",
        layer = urlencode(source.layer),
        attribute = source.attribute,
    )
}

/// Percent-encode the characters a WFS layer name actually contains — spaces and
/// the workspace colon. Not a general encoder and deliberately not: the only
/// inputs are the `layer` strings in this file, and a test checks them.
fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            ':' => "%3A".to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// The RD box to ask for a cell, from its corners **and** its edge midpoints.
///
/// RD's grid is rotated against Web Mercator's by the meridian convergence, so
/// the extreme easting along a cell's north edge is not necessarily at a corner
/// and a box that only spans the corners can miss ground the cell will ask about.
/// `terrain::field_request` takes the same care for the same reason.
fn cell_rd_bbox(cx: i64, cy: i64) -> [f64; 4] {
    let (north, west) = tile_to_lat_lng(cx, cy, REGISTER_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, REGISTER_ZOOM);
    let mut min = [f64::MAX, f64::MAX];
    let mut max = [f64::MIN, f64::MIN];
    for lat in [south, (south + north) / 2.0, north] {
        for lng in [west, (west + east) / 2.0, east] {
            let rd = wgs84_to_rd(lat, lng);
            min = [min[0].min(rd[0]), min[1].min(rd[1])];
            max = [max[0].max(rd[0]), max[1].max(rd[1])];
        }
    }
    [min[0], min[1], max[0], max[1]]
}

/// A feature's properties, as the reader closures see them.
///
/// A borrowed view rather than a map of owned strings: a dense cell is up to
/// 12,000 features and each reader takes one field from each.
pub struct Fields<'a>(&'a serde_json::Map<String, serde_json::Value>);

impl Fields<'_> {
    #[must_use]
    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(serde_json::Value::as_str)
    }

    /// A number the register may have written as a number *or* as a string —
    /// Groningen does both across its layers, and a reader should not have to
    /// know which.
    ///
    /// Unused by the two sources here, which both read text, and kept because the
    /// bug it exists to prevent has already been shipped once in this tree: the
    /// `housenumber` reader in `systems::world_places` looked only for a string
    /// value and silently dropped most house numbers, which arrive as numbers. A
    /// register that publishes a height as `12.5` rather than `"12.5"` would fail
    /// the same way, and the first person to hit it would debug the register.
    #[allow(dead_code)]
    #[must_use]
    pub fn f32(&self, key: &str) -> Option<f32> {
        let v = self.0.get(key)?;
        v.as_f64()
            .map(|n| n as f32)
            .or_else(|| v.as_str()?.trim().parse().ok())
    }
}

/// A GeoServer GeoJSON page of RD points.
///
/// Shared by every source on a GeoServer that answers in RD; what differs per
/// source is the closure in its [`Yields`], which is the only thing that knows a
/// field name.
fn parse_geoserver_rd(text: &str, yields: &Yields) -> Result<RegisteredCell, String> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        features: Vec<Feature>,
        #[serde(rename = "numberReturned", default)]
        returned: usize,
        #[serde(rename = "numberMatched", default)]
        matched: usize,
    }
    #[derive(serde::Deserialize)]
    struct Feature {
        geometry: Option<Geometry>,
        #[serde(default)]
        properties: serde_json::Map<String, serde_json::Value>,
    }
    #[derive(serde::Deserialize)]
    struct Geometry {
        #[serde(default)]
        coordinates: Vec<f64>,
    }

    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if wire.matched > wire.returned && wire.returned > 0 {
        // The ceiling is a guard against an unbounded download, not a sampling
        // rule, and GeoServer's page order is arbitrary — so a cell that hits it
        // loses objects in no particular pattern. Say so, rather than let a
        // thinned avenue read as the register's own answer.
        tracing::warn!(
            returned = wire.returned,
            matched = wire.matched,
            "municipal register truncated a cell at the per-source ceiling"
        );
    }
    // Exact duplicate positions are ghost *versions* of one object, the fault
    // `surveyed::parse_surveyed_cell` guards against: two identical meshes in one
    // place z-fight their own surfaces.
    let mut seen = std::collections::HashSet::new();
    let mut cell = RegisteredCell::default();
    for f in wire.features {
        let Some(g) = f.geometry else { continue };
        let (Some(&x), Some(&y)) = (g.coordinates.first(), g.coordinates.get(1)) else {
            continue;
        };
        if !seen.insert((x.to_bits(), y.to_bits())) {
            continue;
        }
        let fields = Fields(&f.properties);
        let (lat, lng) = rd_to_wgs84(x, y);
        match yields {
            Yields::Trees { height } => cell.trees.push(RegisteredTree {
                lat,
                lng,
                height_m: height(fields),
            }),
            Yields::Props { kind } => {
                if let Some(placed) = kind(fields) {
                    cell.props.push(RegisteredProp {
                        lat,
                        lng,
                        kind: placed.kind,
                        paint: placed.paint,
                    });
                }
            }
        }
    }
    cell.trees.truncate(MAX_PER_SOURCE_PER_CELL);
    cell.props.truncate(MAX_PER_SOURCE_PER_CELL);
    Ok(cell)
}

/// A Dutch height band — `"15 tot 18 m."`, `"tot 6 m."`, `"24 m. en hoger"` — as
/// one number in metres.
///
/// Read off the numbers in the string rather than matched against a table of the
/// eight spellings seen today, because the register is a city's operational
/// database and a ninth band costs nothing to add there. The two open bands are
/// the only judgement in here: the lowest is taken at three quarters of its
/// ceiling, since a tree small enough to sit at the bottom of it is a sapling the
/// register does not carry, and the highest a little above its floor.
fn height_band_m(band: &str) -> Option<f32> {
    let mut nums = band
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<f32>().ok());
    let first = nums.next()?;
    match nums.next() {
        Some(second) => Some((first + second) / 2.0),
        None if band.contains("hoger") => Some(first * 1.15),
        None => Some(first * 0.75),
    }
}

// ─── The normalised form ─────────────────────────────────────────────────────

/// `{"t":[[lat,lng,height|null],…],"p":[[lat,lng,"kind",paint|null],…]}` — what the cache
/// holds, what the fingerprint is taken over, and the only shape anything outside
/// a register's own parser ever sees.
///
/// A prop's kind is a **name**, not an index into a table. An index would be a
/// second wire order to keep append-only forever (`surveyed::PROP_KINDS` already
/// is one, and it is pinned to cartopy's extractor); a name costs six bytes, and
/// a kind this build no longer knows simply drops out on the next read.
///
/// Built with one `String` and `write!` rather than a `format!` per object: a
/// dense cell is up to 12,000 of them.
fn write_normalised(cell: &RegisteredCell) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(cell.trees.len() * 34 + cell.props.len() * 34 + 16);
    out.push_str("{\"t\":[");
    for (i, t) in cell.trees.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match t.height_m {
            Some(h) => {
                let _ = write!(out, "[{:.7},{:.7},{h:.1}]", t.lat, t.lng);
            }
            None => {
                let _ = write!(out, "[{:.7},{:.7},null]", t.lat, t.lng);
            }
        }
    }
    out.push_str("],\"p\":[");
    for (i, p) in cell.props.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "[{:.7},{:.7},\"{}\",",
            p.lat,
            p.lng,
            prop_token(p.kind)
        );
        match p.paint {
            Some(c) => {
                let _ = write!(out, "{c}]");
            }
            None => out.push_str("null]"),
        }
    }
    out.push_str("]}");
    out
}

fn parse_normalised(text: &str) -> Result<RegisteredCell, String> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        t: Vec<(f64, f64, Option<f32>)>,
        #[serde(default)]
        p: Vec<(f64, f64, String, Option<u32>)>,
    }
    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(RegisteredCell {
        trees: wire
            .t
            .into_iter()
            .map(|(lat, lng, height_m)| RegisteredTree { lat, lng, height_m })
            .collect(),
        props: wire
            .p
            .into_iter()
            .filter_map(|(lat, lng, token, paint)| {
                prop_kind(&token).map(|kind| RegisteredProp {
                    lat,
                    lng,
                    kind,
                    paint,
                })
            })
            .collect(),
    })
}

/// The cache's name for a prop kind. Only the kinds a register can supply need
/// one — an unnamed kind cannot be written, so it cannot be read back wrong.
fn prop_token(kind: FurnitureKind) -> &'static str {
    match kind {
        FurnitureKind::Lamp => "lamp",
        FurnitureKind::Bench => "bench",
        FurnitureKind::Bin => "bin",
        FurnitureKind::Recycling => "recycling",
        FurnitureKind::Shelter => "shelter",
        FurnitureKind::Fountain => "fountain",
        FurnitureKind::Swing => "swing",
        FurnitureKind::Cabinet => "cabinet",
        FurnitureKind::Board => "board",
        FurnitureKind::Artwork => "artwork",
        // Not reachable from any register: these are inferred forms, not
        // surveyed ones. Naming them would invite a source to claim it had
        // surveyed a parked car.
        _ => "",
    }
}

fn prop_kind(token: &str) -> Option<FurnitureKind> {
    Some(match token {
        "lamp" => FurnitureKind::Lamp,
        "bench" => FurnitureKind::Bench,
        "bin" => FurnitureKind::Bin,
        "recycling" => FurnitureKind::Recycling,
        "shelter" => FurnitureKind::Shelter,
        "fountain" => FurnitureKind::Fountain,
        "swing" => FurnitureKind::Swing,
        "cabinet" => FurnitureKind::Cabinet,
        "board" => FurnitureKind::Board,
        "artwork" => FurnitureKind::Artwork,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cell that covers the study quay — the case this module exists for.
    const STUDY_CELL: (i64, i64) = (8491, 5320);

    #[test]
    fn groningen_covers_its_own_streets_and_not_utrecht() {
        assert_eq!(
            register_for(STUDY_CELL.0, STUDY_CELL.1).map(|r| r.id),
            Some("groningen")
        );
        // The cell over Utrecht (52.0907, 5.1214). No register yet, and the
        // lookup must say so rather than reach for the nearest one — a city with
        // no entry falls back to the BGT, which is why both are kept.
        assert!(register_for(8425, 5405).is_none());
    }

    /// Suppression is per kind. Groningen supplies trees and lamp posts and says
    /// nothing about benches, so the BGT's benches must survive over its ground —
    /// suppressing the whole cell would delete them in order to fix the trees.
    #[test]
    fn a_register_supersedes_only_what_it_supplies() {
        let (cx, cy) = STUDY_CELL;
        assert!(supersedes(cx, cy, Supply::Trees));
        assert!(supersedes(cx, cy, Supply::Prop(FurnitureKind::Lamp)));
        assert!(supersedes(cx, cy, Supply::Prop(FurnitureKind::Recycling)));
        // Groningen registers no benches, bins or shelters, so those keep coming
        // from the national register and the tile over its ground.
        assert!(!supersedes(cx, cy, Supply::Prop(FurnitureKind::Bench)));
        assert!(!supersedes(cx, cy, Supply::Prop(FurnitureKind::Bin)));
        assert!(!supersedes(8425, 5405, Supply::Trees));
    }

    /// Two registers claiming one piece of ground is the case the whole design
    /// avoids: `register_for` would pick by list order, the loser's city would be
    /// silently unreachable, and the single breaker would gate a service that was
    /// never the failing one.
    #[test]
    fn no_two_registers_claim_the_same_ground() {
        for (i, a) in REGISTERS.iter().enumerate() {
            for b in &REGISTERS[i + 1..] {
                let lat_overlap = a.bounds.0 <= b.bounds.2 && b.bounds.0 <= a.bounds.2;
                let lng_overlap = a.bounds.1 <= b.bounds.3 && b.bounds.1 <= a.bounds.3;
                assert!(
                    !(lat_overlap && lng_overlap),
                    "{} and {} claim overlapping ground",
                    a.id,
                    b.id
                );
            }
        }
    }

    /// Source ids are cache-key segments, so a duplicate would have two layers
    /// overwriting one another's cells — which reads as a layer that is
    /// intermittently empty, not as a mistake.
    #[test]
    fn source_ids_are_unique_within_a_register() {
        for r in REGISTERS {
            for (i, a) in r.sources.iter().enumerate() {
                for b in &r.sources[i + 1..] {
                    assert_ne!(a.id, b.id, "{} has two sources called {}", r.id, a.id);
                }
            }
        }
    }

    /// Every kind a source claims to supply must have a cache name, or its props
    /// are written as `""` and vanish on the next read — a layer that works until
    /// you walk away and come back.
    #[test]
    fn every_supplied_prop_kind_has_a_cache_name() {
        for r in REGISTERS {
            for s in r.sources {
                for supply in s.supplies {
                    if let Supply::Prop(kind) = supply {
                        let token = prop_token(*kind);
                        assert!(!token.is_empty(), "{kind:?} has no cache name");
                        assert_eq!(prop_kind(token), Some(*kind));
                    }
                }
            }
        }
    }

    /// The fixture is the fingerprint's contract, so every source must actually
    /// read something out of it. A source whose branch the fixture never
    /// exercises has a fingerprint that cannot move when that branch changes.
    #[test]
    fn every_source_reads_the_fixture() {
        for r in REGISTERS {
            for s in r.sources {
                let cell = (r.parse)(r.fingerprint_page, &s.yields)
                    .unwrap_or_else(|e| panic!("{}/{} fixture: {e}", r.id, s.id));
                assert!(
                    !cell.is_empty(),
                    "{}/{} reads nothing out of its register's fixture",
                    r.id,
                    s.id
                );
                assert!(
                    r.fingerprint_page.contains(s.attribute),
                    "{}/{} reads {} and the fixture does not carry it",
                    r.id,
                    s.id,
                    s.attribute
                );
            }
        }
    }

    /// The property the whole cache design rests on: change what a reader *does*
    /// and its key changes with it, so stale entries are never served. Checked by
    /// fingerprinting a deliberately altered reader against the real one.
    #[test]
    fn the_fingerprint_moves_when_a_reader_changes() {
        let real = source_fingerprint(&GRONINGEN, &GRONINGEN.sources[0]);

        // Same layer and attribute, different height mapping.
        let altered = Source {
            yields: Yields::Trees {
                height: |f| f.str("BOOMHOOGTE").and_then(height_band_m).map(|h| h + 1.0),
            },
            ..copy_of(&GRONINGEN.sources[0])
        };
        assert_ne!(
            real,
            source_fingerprint(&GRONINGEN, &altered),
            "a changed height reading must move the key"
        );

        // Same reader, different layer name.
        let renamed = Source {
            layer: "geo-data:Bomen gemeente Groningen v2",
            ..copy_of(&GRONINGEN.sources[0])
        };
        assert_ne!(
            real,
            source_fingerprint(&GRONINGEN, &renamed),
            "a changed layer name must move the key"
        );

        // And a reader that is untouched must not move, or every release would
        // re-download the country.
        assert_eq!(real, source_fingerprint(&GRONINGEN, &GRONINGEN.sources[0]));
    }

    /// `Source` holds a function pointer, so it is not `Clone`; the struct-update
    /// syntax above needs a fresh one to fill the fields it does not name.
    fn copy_of(s: &'static Source) -> Source {
        Source {
            id: s.id,
            layer: s.layer,
            attribute: s.attribute,
            yields: match s.yields {
                Yields::Trees { height } => Yields::Trees { height },
                Yields::Props { kind } => Yields::Props { kind },
            },
            supplies: s.supplies,
        }
    }

    /// A layer name carries a space and a colon and goes into a query string.
    #[test]
    fn a_layer_name_is_escaped_into_the_url() {
        let url = groningen_url(
            &GRONINGEN.sources[0],
            STUDY_CELL.0,
            STUDY_CELL.1,
        );
        assert!(
            url.contains("geo-data%3ABomen%20gemeente%20Groningen"),
            "{url}"
        );
        assert!(
            !url.contains("Bomen gemeente"),
            "a raw space reached the url"
        );
        // Each source asks for its own attribute and nothing else.
        assert!(url.contains("propertyName=GEOM%2CBOOMHOOGTE"), "{url}");
    }

    #[test]
    fn a_cells_request_box_is_in_rd_metres_and_contains_the_cell() {
        let [x0, y0, x1, y1] = cell_rd_bbox(STUDY_CELL.0, STUDY_CELL.1);
        // the study quay itself, from the register's own coordinates.
        let rd = wgs84_to_rd(53.225_114_0, 6.578_130_6);
        assert!(
            x0 < rd[0] && rd[0] < x1,
            "easting {} not in {x0}..{x1}",
            rd[0]
        );
        assert!(
            y0 < rd[1] && rd[1] < y1,
            "northing {} not in {y0}..{y1}",
            rd[1]
        );
        // A z14 cell up here is ~1.5 km across; a box wildly bigger than that
        // would be a projection mistake answering with half the city.
        assert!((x1 - x0) < 2_500.0 && (y1 - y0) < 2_500.0);
    }

    /// Every band the register uses today, plus the two open ones, which are the
    /// only places this function decides anything.
    #[test]
    fn height_bands_read_as_metres() {
        assert_eq!(height_band_m("6 tot 9 m."), Some(7.5));
        assert_eq!(height_band_m("15 tot 18 m."), Some(16.5));
        assert_eq!(height_band_m("18 tot 24 m."), Some(21.0));
        assert_eq!(height_band_m("tot 6 m."), Some(4.5));
        assert_eq!(height_band_m("24 m. en hoger"), Some(27.599_998));
        assert_eq!(height_band_m(""), None);
        assert_eq!(height_band_m("onbekend"), None);
    }

    /// A real tree page, trimmed: RD point geometry, a band, a null band, and a
    /// duplicate of the first.
    #[test]
    fn a_tree_page_parses_into_normalised_trees() {
        let page = r#"{"type":"FeatureCollection","numberReturned":3,"numberMatched":3,
            "features":[
              {"geometry":{"type":"Point","coordinates":[234729.772,582447.843]},
               "properties":{"BOOMHOOGTE":"15 tot 18 m."}},
              {"geometry":{"type":"Point","coordinates":[234800.0,582500.0]},
               "properties":{"BOOMHOOGTE":null}},
              {"geometry":{"type":"Point","coordinates":[234729.772,582447.843]},
               "properties":{"BOOMHOOGTE":"15 tot 18 m."}}]}"#;
        let cell = parse_geoserver_rd(page, &GRONINGEN.sources[0].yields).expect("a valid page");
        assert_eq!(cell.trees.len(), 2, "the repeated position is one tree");
        assert!(cell.props.is_empty());
        assert_eq!(cell.trees[0].height_m, Some(16.5));
        assert_eq!(cell.trees[1].height_m, None);
        // The register publishes this tree's own longitude and latitude beside
        // the geometry; the conversion has to land on them.
        assert!((cell.trees[0].lat - 53.222_694_63).abs() < 1e-5);
        assert!((cell.trees[0].lng - 6.580_966_22).abs() < 1e-5);
    }

    /// The lighting layer, whose whole point is that it refuses most of what it
    /// is sent: a ground spot is not a lamp post.
    #[test]
    fn a_lamp_page_keeps_columns_and_refuses_the_rest() {
        let page = r#"{"numberReturned":4,"numberMatched":4,"features":[
            {"geometry":{"type":"Point","coordinates":[234729.0,582447.0]},
             "properties":{"LICHTPUNT_SOORT":"Lichtmast"}},
            {"geometry":{"type":"Point","coordinates":[234730.0,582448.0]},
             "properties":{"LICHTPUNT_SOORT":"Grondspot"}},
            {"geometry":{"type":"Point","coordinates":[234731.0,582449.0]},
             "properties":{"LICHTPUNT_SOORT":"Klok"}},
            {"geometry":{"type":"Point","coordinates":[234732.0,582450.0]},
             "properties":{"LICHTPUNT_SOORT":"Lichtmast"}}]}"#;
        let cell = parse_geoserver_rd(page, &GRONINGEN.sources[1].yields).expect("a valid page");
        assert_eq!(cell.props.len(), 2, "only the two columns");
        assert!(cell.trees.is_empty());
        assert!(cell.props.iter().all(|p| p.kind == FurnitureKind::Lamp));
    }

    /// Containers: one form, four colours. The fraction is what says what goes
    /// in the thing, and it is the reason `paint` exists on the wire at all.
    #[test]
    fn a_container_page_colours_each_fraction() {
        let page = r#"{"numberReturned":5,"numberMatched":5,"features":[
            {"geometry":{"type":"Point","coordinates":[234231.0,582950.0]},
             "properties":{"FRACTIE":"RESTAFVAL"}},
            {"geometry":{"type":"Point","coordinates":[234232.0,582951.0]},
             "properties":{"FRACTIE":"PAPIER"}},
            {"geometry":{"type":"Point","coordinates":[234233.0,582952.0]},
             "properties":{"FRACTIE":"GLAS"}},
            {"geometry":{"type":"Point","coordinates":[234234.0,582953.0]},
             "properties":{"FRACTIE":"TEXTIEL"}},
            {"geometry":{"type":"Point","coordinates":[234235.0,582954.0]},
             "properties":{"FRACTIE":"ONBEKEND"}}]}"#;
        let source = GRONINGEN
            .sources
            .iter()
            .find(|s| s.id == "containers")
            .expect("a container source");
        let cell = parse_geoserver_rd(page, &source.yields).expect("a valid page");
        assert_eq!(
            cell.props.len(),
            5,
            "an unknown fraction is still a container"
        );
        assert!(cell
            .props
            .iter()
            .all(|p| p.kind == FurnitureKind::Recycling));
        let colours: Vec<_> = cell.props.iter().map(|p| p.paint).collect();
        // Household waste keeps the steel a bottle bank was already drawn in, so
        // the commonest container by far does not change appearance at all.
        assert_eq!(colours[0], Some(0x9A_98_93));
        assert_eq!(colours[4], Some(0x9A_98_93), "unknown falls back to steel");
        // The three sorted fractions are each their own colour, and distinct.
        let sorted = [colours[1], colours[2], colours[3]];
        assert_eq!(
            sorted
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3,
            "paper, glass and textile must not share a colour"
        );
        assert!(sorted.iter().all(|c| *c != Some(0x9A_98_93)));
    }

    /// A charge point layer whose vocabulary is one word today. Anything else is
    /// refused, because a post drawn where a planned unit will go is worse than a
    /// missing one.
    #[test]
    fn a_charger_page_keeps_only_existing_posts() {
        let page = r#"{"numberReturned":3,"numberMatched":3,"features":[
            {"geometry":{"type":"Point","coordinates":[235022.0,582977.0]},
             "properties":{"STATUS_LAADPAAL":"Bestaande laadpaal"}},
            {"geometry":{"type":"Point","coordinates":[235023.0,582978.0]},
             "properties":{"STATUS_LAADPAAL":"Ingetrokken"}},
            {"geometry":{"type":"Point","coordinates":[235024.0,582979.0]},
             "properties":{"STATUS_LAADPAAL":null}}]}"#;
        let source = GRONINGEN
            .sources
            .iter()
            .find(|s| s.id == "chargers")
            .expect("a charger source");
        let cell = parse_geoserver_rd(page, &source.yields).expect("a valid page");
        assert_eq!(cell.props.len(), 1);
        assert_eq!(cell.props[0].kind, FurnitureKind::Cabinet);
    }

    /// The cache round trip, both shapes at once. A height that survives as
    /// `None` is the case that would otherwise turn every unmeasured tree into a
    /// zero-metre one.
    #[test]
    fn the_normalised_form_round_trips() {
        let cell = RegisteredCell {
            trees: vec![
                RegisteredTree {
                    lat: 53.222_694_6,
                    lng: 6.580_966_2,
                    height_m: Some(16.5),
                },
                RegisteredTree {
                    lat: 53.225_114_0,
                    lng: 6.578_130_6,
                    height_m: None,
                },
            ],
            props: vec![
                RegisteredProp {
                    lat: 53.225_2,
                    lng: 6.578_2,
                    kind: FurnitureKind::Lamp,
                    paint: None,
                },
                // A colour has to survive the round trip too: a bottle bank that
                // came back grey would be the fraction silently lost.
                RegisteredProp {
                    lat: 53.225_3,
                    lng: 6.578_3,
                    kind: FurnitureKind::Recycling,
                    paint: Some(0x3E_5E_8C),
                },
            ],
        };
        let back = parse_normalised(&write_normalised(&cell)).expect("round trip");
        assert_eq!(back.trees.len(), 2);
        assert_eq!(back.trees[0].height_m, Some(16.5));
        assert_eq!(back.trees[1].height_m, None);
        assert_eq!(back.props.len(), 2);
        assert_eq!(back.props[0].kind, FurnitureKind::Lamp);
        assert_eq!(back.props[0].paint, None);
        assert_eq!(back.props[1].kind, FurnitureKind::Recycling);
        assert_eq!(back.props[1].paint, Some(0x3E_5E_8C));
        assert!((back.trees[0].lat - cell.trees[0].lat).abs() < 1e-6);
    }

    #[test]
    fn a_page_that_is_not_json_is_an_error_not_an_empty_cell() {
        assert!(
            parse_geoserver_rd("<ows:ExceptionReport/>", &GRONINGEN.sources[0].yields).is_err()
        );
    }
}
