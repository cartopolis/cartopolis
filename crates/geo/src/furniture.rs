//! Street furniture extraction and meshing — the pure half.
//!
//! No Bevy: this turns a tile's point features into [`FurnitureInstance`]s and
//! then into meshes. The ECS half — the distance LOD and the materials — is
//! `cartopolis`'s `systems::street_furniture`.
//!
//! Street furniture — benches, bins, shelters, picnic tables — read out of the
//! vector tile the map is already made of.
//!
//! This is the cheapest realism there is: `map_geometry` fetches one MVT body per
//! cell and Shortbread's **`pois`** layer is already inside it (467 features in the
//! Groningen z14 fixture, against 8,552 buildings and 95 streets). Nothing here
//! costs a request. Until now the whole layer was decoded and dropped —
//! `vector_tiles::PAINT_ORDER` skips it as a *label* layer, which it is on a paper
//! map and is not in a world you stand in.
//!
//! Three things shape the design, and two of them are inherited:
//!
//! * **`pois` has no `kind`.** Every other Shortbread layer discriminates on one
//!   `kind` property; this one carries the raw OSM tags through instead
//!   (`amenity`, `leisure`, `tourism`, `shop`, `man_made`, `information`, …), so
//!   [`furniture_kind`] matches on key *and* value. [`FURNITURE_TABLE`] is the
//!   whole vocabulary, and anything unlisted gets **nothing** — the same "an
//!   unstyled polygon beats a wrongly styled one" rule the fills and the tree
//!   scatter follow. Most of the layer is shops, doctors and restaurants: indoor
//!   things whose node is a point inside a building we already draw.
//! * **The map is pale cartographic paper** (see `systems::vegetation`), so the
//!   tones here are the muted timber/grey family of [`palette::TRUNK`] and
//!   [`palette::BUILDING_EDGE`], never the primary colours real playground
//!   equipment is painted in. A saturated bench at walking distance is the
//!   loudest thing on screen.
//! * **Mass mesh churn loses the device** (`docs/notes/upload-bursts.md`).
//!   Furniture is merged into *one* mesh per cell, rides `map_geometry`'s existing
//!   worker, build cap and despawn cap, and is hard-capped per cell by
//!   [`MAX_FURNITURE_PER_CELL`].
//!
//! The part that is genuinely new is orientation. A POI is a bare point: OSM
//! records that a bench exists, never which way it looks. Scattered at a hashed
//! angle a row of benches along a canal reads as litter, so [`WayField`] answers
//! "which way does the nearest street or footpath run, and which side of it am I
//! on" out of the same tile's `streets` layer, and the objects that have a front
//! ([`FurnitureKind::faces_the_path`]) turn to face it. That single number is most
//! of the difference between furniture and boxes.

use std::collections::HashMap;

use mvt_reader::feature::Feature;

use crate::pois::{single_point, tag_str};
use crate::scatter::{bucket_range, hash3, rand01, srgb_to_linear_rgb};
use crate::vector_tiles::{
    palette, SurfaceGroup, SurfaceMesh, TileData, INDEX_BYTES, TILE_BUFFER_FRAC, VERTEX_BYTES,
    WATER_LAYERS,
};

/// Altitude at which furniture starts merging into the paving.
///
/// An order of magnitude lower than [`vegetation::TREE_FADE_START`](crate::systems::vegetation::TREE_FADE_START),
/// because the objects are an order of magnitude smaller: a 1.8 m bench is already
/// under a pixel at the altitude where a 12 m tree is still a recognisable blob.
pub const FURNITURE_FADE_START: f32 = 150.0;

/// Altitude by which it has merged completely and the geometry can be hidden.
pub const FURNITURE_FADE_END: f32 = 450.0;

/// Hard ceiling on furniture per streamed cell. A dense city-centre z14 cell runs
/// to a few hundred; this is headroom, not a working limit, and exists for the
/// same reason [`MAX_TREES_PER_CELL`](crate::systems::vegetation::MAX_TREES_PER_CELL)
/// does — one command buffer must not be handed a million triangles. Truncation is
/// logged rather than silent.
pub const MAX_FURNITURE_PER_CELL: usize = 1_500;

/// Radius within which a piece of furniture looks for a path to face. Beyond this
/// there is nothing to orient against and the angle falls back to the hash.
///
/// 20 m is about as far as a bench is ever set back from the way it belongs to;
/// much wider and a bench in the middle of a square starts squaring up to a road
/// it has nothing to do with.
const ORIENT_RADIUS_M: f32 = 20.0;

// ─── What we know how to build ───────────────────────────────────────────────

/// One placed object, in the tessellator's tile-local metre frame (origin at the
/// cell's north-west corner, +x east, +z south, ground at y = 0).
#[derive(Debug, Clone, Copy)]
pub struct FurnitureInstance {
    pub x: f32,
    pub z: f32,
    /// Rotation about +y, radians. See [`WayField`] for where it comes from.
    pub yaw: f32,
    pub kind: FurnitureKind,
    /// Per-instance brightness multiplier, centred on 1.0. The same trick the tree
    /// scatter uses: one shared material, and a street's worth of objects that are
    /// not all the identical grey.
    pub luma: f32,
    /// sRGB body colour for the parts a form paints per instance ([`Tone::Paint`]).
    ///
    /// Every surveyed form is [`palette::FURNITURE`] here, i.e. no tint at all —
    /// a bench is the colour its `Tone`s say it is. It exists for the inferred
    /// ones: a car park's worth of cars in one identical grey is the giveaway of a
    /// fill, and one colour per instance is the cheapest cure that keeps the
    /// group's single shared material (`crate::parking`).
    pub paint: u32,
    /// This instance stands in open water.
    ///
    /// Set by [`extract_furniture`] from the tile's own water polygons, and read by
    /// [`parts_for`] and by the client's collider pass. It exists for one surveyed
    /// thing that is routinely two different objects: `amenity=fountain` is a
    /// plaza basin on a square and a jet in the middle of a pond, and the register
    /// does not distinguish them because from above they are the same point.
    ///
    /// Drawing the basin for both is what put a 3 m ring of masonry at the
    /// waterline of the study pond in Groningen, with a solid collider and an
    /// offer to walk up to it, 40 m out into the pond. A flag rather than a second
    /// [`FurnitureKind`] on purpose: the vocabulary is closed, hand-synced with
    /// the atlas web UI and reachable by an operator's placeholder model through
    /// [`FurnitureKind::slug`], and a jet and a basin should still both be "the
    /// fountain" to all three of those.
    pub over_water: bool,
    /// For a houseboat: draw the hull alone. Where the register's own building
    /// stands on the berth — a `woonark` registered as a pand, which 3DBAG then
    /// models — that building *is* the house, and a second one drawn into it
    /// was a box inside a box (Woonschepenhaven, 2026-10-01). A flag for the
    /// reason [`Self::over_water`] is one. `false` everywhere else.
    pub hull_only: bool,
}

/// The vocabulary of things this module can stand on a street.
///
/// Deliberately short. Each entry is a form that is unmistakable in six boxes at
/// walking distance and harmless at a hundred metres; a kind that would need a
/// texture, a curve or a survey to read correctly is better left undrawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FurnitureKind {
    Bench,
    /// A litter bin.
    Bin,
    /// A bottle bank / recycling container — the big one, not the bin.
    Recycling,
    /// A bus or picnic shelter: posts, a roof and a back panel.
    Shelter,
    PicnicTable,
    /// Post box, vending machine, parcel locker: a box on a plinth.
    Cabinet,
    /// An information board or map on two posts.
    Board,
    /// A basin with water in it.
    Fountain,
    /// A plinth with a form on it — what a `tourism=artwork` node gets.
    Artwork,
    /// A swing frame. The token for `leisure=playground`, whose node is the
    /// *centroid* of a play area we have no geometry for; see [`FURNITURE_TABLE`].
    Swing,
    /// A street light: a column with a head on a short bracket.
    ///
    /// **Not** in [`FURNITURE_TABLE`] either, and for the opposite reason to the
    /// vehicles below — a lamppost is not inferred, it is surveyed, just not by
    /// the source this module reads. Shortbread's `pois` layer does not carry
    /// street lighting at all; the BGT surveys every column in the country, and
    /// `systems::surveyed` lays these from it. It is by a wide margin the most
    /// numerous object on a street: 651 columns against 54 of everything else in
    /// one Groningen page.
    Lamp,
    /// A parked car. **Not** in [`FURNITURE_TABLE`]: no `pois` feature is a car,
    /// and OSM does not survey individual vehicles. It is laid into the `sites`
    /// layer's car parks by [`crate::parking`], which is a different kind of
    /// claim — see that module for what it does and does not assert.
    Car,
    /// A parked bicycle, from the same place and on the same terms.
    Bicycle,
    /// A person on foot. Like the vehicles it is inferred rather than surveyed —
    /// OSM records where people may walk, never who is walking there — and it is
    /// the one form here that is only ever *moving* (`systems::traffic` drives a
    /// pool of them; nothing places a standing one).
    Person,
    /// A boat moored against a canal bank. Like the two above it is inferred
    /// rather than surveyed — see [`crate::moorings`], which reads the shape of
    /// the water polygon rather than any tag to decide there is a canal there at
    /// all.
    Boat,
    /// A traffic signal: a post with a signal head, for cars or for pedestrians.
    /// Surveyed by the city (`systems::municipal`), not by the BGT. Appended
    /// last, so the masks' existing bits keep their places.
    TrafficLight,
    /// A yacht in a marina, moored alongside a jetty ([`crate::moorings`]):
    /// white, with a cabin and a mast — the leisure boats of a harbour, where
    /// the canal boats are dark working hulls.
    Yacht,
    /// A houseboat on a registered berth (the BAG's `ligplaats`): a long flat
    /// hull with a house on it, lying along the bank.
    Houseboat,
    /// A bike rack — the BGT's surveyed `fietsenrek`. A rack, not the bikes in
    /// it: the register says where the rack stands and nothing about how full it
    /// is, so drawing bicycles in it would be inventing them.
    BikeRack,
    /// A public toilet — the BGT's surveyed `openbaar toilet`. Drawn as a cabin;
    /// the register does not say whether it is one or an open urinal.
    Toilet,
}

impl FurnitureKind {
    /// Whether this form takes its angle from the nearest path.
    ///
    /// False for the two forms that belong to an open space rather than to a way:
    /// a fountain and a plinth are close to radially symmetric and stand in the
    /// middle of a square, where squaring up to the nearest side street is a
    /// worse answer than any angle at all.
    #[must_use]
    fn takes_angle_from_path(self) -> bool {
        !matches!(self, Self::Fountain | Self::Artwork)
    }

    /// The name this form travels under when an operator replaces it with a
    /// model (`cartopolis_core::placeholders`).
    ///
    /// Spelled out rather than derived from the variant name: these strings are
    /// in a server's database and in the atlas web UI, so renaming a variant here
    /// must not silently re-point every row that used to match it.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Bench => "bench",
            Self::Bin => "bin",
            Self::Recycling => "recycling",
            Self::Shelter => "shelter",
            Self::PicnicTable => "picnic-table",
            Self::Cabinet => "cabinet",
            Self::Board => "board",
            Self::Fountain => "fountain",
            Self::Artwork => "artwork",
            Self::Swing => "swing",
            Self::Lamp => "lamp",
            Self::Car => "car",
            Self::Bicycle => "bicycle",
            Self::Person => "person",
            Self::Boat => "boat",
            Self::TrafficLight => "traffic-light",
            Self::Yacht => "yacht",
            Self::Houseboat => "houseboat",
            Self::BikeRack => "bike-rack",
            Self::Toilet => "toilet",
        }
    }

    /// The form a slug names, or `None` for one this build does not know.
    ///
    /// `None` rather than a default: a placeholder set against a form that no
    /// longer exists must draw the thing it always drew, not an arbitrary other
    /// object wearing somebody's model.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        ALL_FURNITURE_KINDS
            .iter()
            .copied()
            .find(|kind| kind.slug() == slug)
    }
}

/// A set of forms, small enough to be `Copy` and to sit in a job's parameters.
///
/// A bitmask and not a `HashSet` for one reason: it rides in the client's
/// `DetailWants`, which is compared every frame to decide whether the live cells
/// have to be rebuilt. A set that allocated would make that comparison a heap
/// walk on the frame loop, and — worse — `DetailWants` is `Copy` precisely so it
/// can be handed to a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FurnitureMask(u32);

impl FurnitureMask {
    /// The empty set: every form draws the shape built into this client.
    pub const NONE: Self = Self(0);

    fn bit(kind: FurnitureKind) -> u32 {
        // `position` over the table rather than a discriminant cast: the enum has
        // no explicit values, and a reordering must not silently re-point a bit.
        let index = ALL_FURNITURE_KINDS
            .iter()
            .position(|k| *k == kind)
            .expect("every form is in ALL_FURNITURE_KINDS");
        1 << index
    }

    #[must_use]
    pub fn with(self, kind: FurnitureKind) -> Self {
        Self(self.0 | Self::bit(kind))
    }

    #[must_use]
    pub fn contains(self, kind: FurnitureKind) -> bool {
        self.0 & Self::bit(kind) != 0
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Every form, for the two places that have to walk the whole set: the slug
/// lookup above and the test that pins it against the shared vocabulary.
pub const ALL_FURNITURE_KINDS: &[FurnitureKind] = &[
    FurnitureKind::Bench,
    FurnitureKind::Bin,
    FurnitureKind::Recycling,
    FurnitureKind::Shelter,
    FurnitureKind::PicnicTable,
    FurnitureKind::Cabinet,
    FurnitureKind::Board,
    FurnitureKind::Fountain,
    FurnitureKind::Artwork,
    FurnitureKind::Swing,
    FurnitureKind::Lamp,
    FurnitureKind::Car,
    FurnitureKind::Bicycle,
    FurnitureKind::Person,
    FurnitureKind::Boat,
    FurnitureKind::TrafficLight,
    FurnitureKind::Yacht,
    FurnitureKind::Houseboat,
    FurnitureKind::BikeRack,
    FurnitureKind::Toilet,
];

/// Where a form can be sat on, in the object's own frame (+z front).
///
/// The numbers are read off [`form`]'s parts rather than restated: a seat is the
/// top of a slat that is already meshed, and the two drifting apart would put the
/// avatar's hips through the timber.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seat {
    /// Height of the slat's top surface above the object's ground.
    pub height: f32,
    /// Half the usable length along the object's x axis. Shorter than the slat
    /// itself, so a sitter placed at the end of the span still has both hips on
    /// it.
    pub half_span: f32,
    /// One entry per place to sit: the slat's z in the object frame, and the yaw
    /// (radians about +y, added to the object's own) a sitter turns to. A bench
    /// has one and it faces away from its back panel; a picnic table has two,
    /// facing each other across it.
    pub rows: &'static [(f32, f32)],
}

/// Where the avatar can sit on `kind`, or `None` for a form nobody sits on.
///
/// The verb the prompt offers is derived from the same fact on the client side,
/// so anything answering `Some` here must read "Sit at …" there.
#[must_use]
pub fn seat(kind: FurnitureKind) -> Option<Seat> {
    match kind {
        // form::BENCH[0]: slat centred at y 0.45, 3 cm half-height, z 0. The back
        // panel is at −z, so a sitter faces +z, which is also the side the whole
        // object was turned toward the path.
        FurnitureKind::Bench => Some(Seat {
            height: 0.48,
            half_span: 0.6,
            rows: &[(0.0, 0.0)],
        }),
        // form::PICNIC_TABLE[1..3]: two slats at z ±0.62, same height, each facing
        // the table top between them.
        FurnitureKind::PicnicTable => Some(Seat {
            height: 0.48,
            half_span: 0.6,
            rows: &[(-0.62, 0.0), (0.62, std::f32::consts::PI)],
        }),
        _ => None,
    }
}

/// OSM tag → form. Matched on key **and** value, in order, first hit wins.
///
/// `pois` carries raw OSM tags rather than a Shortbread `kind`, and a feature
/// routinely holds several of them (a `tourism=information` that is also an
/// `information=map`), so this cannot be a lookup on one key.
///
/// What is *not* here matters as much as what is. `amenity=atm`, `telephone` and
/// `defibrillator` are usually mounted on or in a wall, and standing them free in
/// the street would put a box in the middle of a pavement they are not on.
/// Everything without an outdoor, free-standing, roughly-known shape is skipped.
const FURNITURE_TABLE: &[(&str, &str, FurnitureKind)] = &[
    ("amenity", "bench", FurnitureKind::Bench),
    ("amenity", "waste_basket", FurnitureKind::Bin),
    ("amenity", "waste_disposal", FurnitureKind::Recycling),
    ("amenity", "recycling", FurnitureKind::Recycling),
    ("amenity", "shelter", FurnitureKind::Shelter),
    ("amenity", "fountain", FurnitureKind::Fountain),
    ("amenity", "post_box", FurnitureKind::Cabinet),
    ("amenity", "vending_machine", FurnitureKind::Cabinet),
    ("amenity", "parcel_locker", FurnitureKind::Cabinet),
    ("tourism", "picnic_site", FurnitureKind::PicnicTable),
    ("leisure", "picnic_table", FurnitureKind::PicnicTable),
    ("tourism", "artwork", FurnitureKind::Artwork),
    ("information", "map", FurnitureKind::Board),
    ("information", "board", FurnitureKind::Board),
    ("tourism", "information", FurnitureKind::Board),
    // A playground's node is the centroid of an area, so this is a token for the
    // place rather than a survey of its equipment — one swing frame in the middle
    // of the `land` polygon that is already painted as a playground. Standing a
    // whole play park on one point would be inventing geometry; standing nothing
    // leaves the most child-legible place on the map blank.
    ("leisure", "playground", FurnitureKind::Swing),
];

/// Classify one `pois` feature. `None` for everything the table does not name.
#[must_use]
pub fn furniture_kind(feature: &Feature) -> Option<FurnitureKind> {
    FURNITURE_TABLE
        .iter()
        .find(|(key, value, _)| tag_str(feature, key) == Some(value))
        .map(|(_, _, kind)| *kind)
}

// ─── Extraction ──────────────────────────────────────────────────────────────

/// Read a tile's `pois` layer as placed, oriented furniture.
///
/// `seed` must be stable for a given cell (its x/y are the natural choice) so a
/// cell that streams out and back does not re-roll the angle of every unoriented
/// object — the tree scatter documents why a reshuffle is instantly visible.
///
/// Takes the cell's already-parsed [`TileData`] rather than the tile body: every
/// consumer on this worker reads a different layer out of the same bytes, and
/// `Reader::new` is a whole-tile decode (see `vector_tiles::TileData`).
/// Does this tile put water under a tile-local point?
///
/// An exact even-odd test against the tile's own water polygons, walked on demand
/// rather than rasterised: [`crate::vector_tiles::water_shore_field`] answers a
/// richer question (how far to the bank) at 1056² and is built once per *cell*,
/// which is the right shape for shading a canal and the wrong shape for asking
/// about the two or three points in a cell that are fountains.
///
/// Holes count as land — a pond with an island in it has ground on the island — so
/// interior rings flip the answer exactly as the exterior does.
fn water_under(tile: &TileData, tx: f32, ty: f32) -> bool {
    use geo_types::Geometry;

    fn in_ring(ring: impl Iterator<Item = (f64, f64)>, px: f64, py: f64) -> bool {
        let pts: Vec<(f64, f64)> = ring.collect();
        let mut inside = false;
        let mut j = pts.len().wrapping_sub(1);
        for i in 0..pts.len() {
            let (xi, yi) = pts[i];
            let (xj, yj) = pts[j];
            if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
        inside
    }

    let (px, py) = (f64::from(tx), f64::from(ty));
    for meta in tile.layers() {
        if !WATER_LAYERS.contains(&meta.name.as_str()) {
            continue;
        }
        // Each layer declares its own extent, so a point in the `pois` frame has to
        // be carried into this one. Shortbread emits 4096 everywhere and this is a
        // no-op there; the `WayField` trap in this module is what it costs to
        // assume that.
        let extent = f64::from(TileData::extent(meta));
        let (qx, qy) = (px * extent, py * extent);
        for feature in tile.features(meta) {
            let hit = |poly: &geo_types::Polygon<f32>| {
                let mut inside = in_ring(
                    poly.exterior()
                        .coords()
                        .map(|c| (f64::from(c.x), f64::from(c.y))),
                    qx,
                    qy,
                );
                if inside {
                    for hole in poly.interiors() {
                        if in_ring(
                            hole.coords().map(|c| (f64::from(c.x), f64::from(c.y))),
                            qx,
                            qy,
                        ) {
                            inside = false;
                            break;
                        }
                    }
                }
                inside
            };
            let wet = match &feature.geometry {
                Geometry::Polygon(p) => hit(p),
                Geometry::MultiPolygon(mp) => mp.iter().any(|p| hit(p)),
                _ => false,
            };
            if wet {
                return true;
            }
        }
    }
    false
}

pub fn extract_furniture(
    tile: &TileData,
    tile_m: f32,
    seed: u64,
) -> Result<Vec<FurnitureInstance>, String> {
    let Some(meta) = tile.layer("pois") else {
        return Ok(Vec::new());
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;

    // `tile_m`, not this layer's `scale`/`extent`: [`WayField`] reads the
    // *`streets`* layer, and MVT declares an extent per layer. Handing it the
    // `pois` numbers happened to work only because Shortbread emits 4096
    // everywhere — on any tile where the two differ, every bench took its angle
    // from a street field scaled by the wrong factor, i.e. from streets that were
    // nowhere near it.
    let ways = WayField::build(tile, tile_m);

    let mut out = Vec::new();
    let mut truncated = false;

    for (fi, feature) in features.iter().enumerate() {
        let Some(kind) = furniture_kind(feature) else {
            continue;
        };
        // MVT layers carry a buffer past the tile edge, so the same bench appears
        // in two neighbouring tiles. Owning exactly `[0, extent)` is the dedup:
        // every point lands in exactly one cell, with no tolerance to tune.
        let Some((tx, ty)) = single_point(&feature.geometry) else {
            continue;
        };
        if tx < 0.0 || ty < 0.0 || tx >= extent || ty >= extent {
            continue;
        }
        let (x, z) = (tx * scale, ty * scale);

        let h = hash3(seed, fi as i32, kind as i32);
        let yaw = kind
            .takes_angle_from_path()
            .then(|| ways.facing(x, z, ORIENT_RADIUS_M))
            .flatten()
            // Nothing to line up with: a hashed angle beats a grid of objects all
            // squared to Mercator north.
            .unwrap_or_else(|| rand01(h, 0) * std::f32::consts::TAU);

        // Asked only of the kind it changes, and that is not thrift for its own
        // sake: the test walks every water ring in the tile, a cell holds two or
        // three fountains and several hundred benches, and a bench's form does not
        // depend on the answer.
        let over_water =
            matches!(kind, FurnitureKind::Fountain) && water_under(tile, tx / extent, ty / extent);

        out.push(FurnitureInstance {
            x,
            z,
            yaw,
            kind,
            luma: 1.0 + (rand01(h, 1) - 0.5) * 0.10,
            paint: default_paint(kind),
            over_water,
            hull_only: false,
        });
        if out.len() >= MAX_FURNITURE_PER_CELL {
            truncated = true;
            break;
        }
    }

    if truncated {
        tracing::warn!(
            cap = MAX_FURNITURE_PER_CELL,
            "street furniture hit the per-cell cap; this cell is furnished short"
        );
    }
    Ok(out)
}

// ─── Which way does the street run? ──────────────────────────────────────────

/// Which `streets` features a [`WayField`] admits.
///
/// The filter is on the *feature*, not on the segment, so it is applied once per
/// way rather than once per vertex pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WayFilter {
    /// Every street, path and railway — what the furniture orients against. A
    /// bench beside a tram line is beside a way like any other.
    All,
    /// Everything [`road_texture::is_rail_kind`](crate::road_texture::is_rail_kind)
    /// calls track. [`crate::crossings`] is the caller and the reason: the
    /// register puts a level crossing's point *on the rail centreline*, so the
    /// nearest way is the track every time, and a boom laid along the track is
    /// the one orientation that is certainly wrong.
    ExcludingRail,
}

impl WayFilter {
    fn admits(self, kind: &str) -> bool {
        match self {
            Self::All => true,
            Self::ExcludingRail => !crate::road_texture::is_rail_kind(kind),
        }
    }

    /// [`Self::admits`] over a whole feature, reading the `kind` tag **only when
    /// the answer depends on it**. `All` is what the furniture uses on every
    /// streamed cell, and it must cost exactly what it did before this filter
    /// existed: no tag lookup per way.
    fn admits_feature(self, feature: &Feature) -> bool {
        match self {
            Self::All => true,
            Self::ExcludingRail => self.admits(tag_str(feature, "kind").unwrap_or_default()),
        }
    }

    /// Half the carriageway this client *draws* for the feature, metres — the
    /// second thing a filtered lookup can answer, beside the direction.
    ///
    /// Only for a filter that already reads the tags: `All` answers `None`
    /// unconditionally, which is what keeps the rule above true. `None` also
    /// means "the width is not known" —
    /// [`drawn_street`](crate::vector_tiles::drawn_street) refuses a tunnel and
    /// any kind with no road class, and both fall back exactly as an untagged
    /// way does.
    fn half_width(self, feature: &Feature) -> Option<f32> {
        match self {
            Self::All => None,
            Self::ExcludingRail => crate::vector_tiles::drawn_street(feature).map(|(_, half)| half),
        }
    }
}

/// Every street, path and railway in the tile — or every one the [`WayFilter`]
/// admits — bucketed on a uniform grid so "which way does the nearest one run"
/// costs a 3×3 cell lookup rather than a scan over the layer.
///
/// Segments are taken **unclipped**. The clip that
/// [`clipped_rings`](crate::systems::vector_tiles::clipped_rings) exists for
/// matters when a shape's *area* is being filled; here only a direction is read
/// off, and a segment that runs out past the tile edge points the same way
/// whether or not its far end is trimmed. What is dropped instead is any segment
/// with an endpoint outside the tile's own buffer window — which is a bound on
/// the *index*, not a cartographic judgement, and the reason is in
/// [`TILE_BUFFER_FRAC`].
pub(crate) struct WayField {
    segs: Vec<([f32; 2], [f32; 2])>,
    /// Half the carriageway of the way each segment came off, metres, parallel
    /// to [`Self::segs`]. `None` where the way's `kind` was never read (the
    /// [`WayFilter::All`] path) or is not drawn as a carriageway at all.
    halves: Vec<Option<f32>>,
    buckets: HashMap<(i32, i32), Vec<u32>>,
    /// The cell's ground size, which is what bounds the bucket grid — see
    /// [`bucket_range`](crate::scatter::bucket_range).
    tile_m: f32,
}

impl WayField {
    const CELL: f32 = 24.0;

    /// Build the field from the tile's `streets` layer.
    ///
    /// Takes `tile_m` and derives the scale from the *streets* layer's own
    /// extent. It used to be handed the caller's `scale`/`extent`, which is the
    /// `pois` layer's — see [`extract_furniture`].
    fn build(tile: &TileData, tile_m: f32) -> Self {
        Self::build_with(tile, tile_m, WayFilter::All)
    }

    /// An empty field, for a caller that pushes its own ways.
    pub(crate) fn empty(tile_m: f32) -> Self {
        Self {
            segs: Vec::new(),
            halves: Vec::new(),
            buckets: HashMap::new(),
            tile_m,
        }
    }

    /// [`Self::build`], with a say in which features count.
    pub(crate) fn build_with(tile: &TileData, tile_m: f32, filter: WayFilter) -> Self {
        use geo_types::Geometry;

        let mut field = Self::empty(tile_m);
        let Some(meta) = tile.layer("streets") else {
            return field;
        };
        let features = tile.features(meta);
        let extent = TileData::extent(meta);
        let scale = tile_m / extent;

        let margin = extent * TILE_BUFFER_FRAC;
        // Written as a positive test, so a NaN — false against every comparison —
        // is rejected rather than admitted.
        let in_range = |c: &geo_types::Coord<f32>| {
            c.x >= -margin && c.y >= -margin && c.x <= extent + margin && c.y <= extent + margin
        };
        for feature in features {
            // Once per way, not once per segment. An untagged feature has no
            // `kind` at all, and `""` is not a rail kind — so it stays in, which
            // is the answer this loop gave before the filter existed.
            if !filter.admits_feature(feature) {
                continue;
            }
            // Once per way too, and only on a filter that is already paying for
            // the tags — see [`WayFilter::half_width`].
            let half = filter.half_width(feature);
            let lines: Vec<&geo_types::LineString<f32>> = match &feature.geometry {
                Geometry::LineString(ls) => vec![ls],
                Geometry::MultiLineString(ml) => ml.iter().collect(),
                _ => continue,
            };
            for line in lines {
                for w in line.0.windows(2) {
                    // *Both* endpoints, where this used to demand that both be
                    // out of range before it dropped anything. One wild end was
                    // enough: `mvt-reader` clips its cursor at `i32::MAX` and
                    // never to the extent (see `TILE_BUFFER_FRAC`), so a segment
                    // running from a plausible bench-side vertex to ~2.1e9 was
                    // accepted, and the bbox loop in `push` then tried to insert
                    // on the order of 10^14 buckets.
                    if !in_range(&w[0]) || !in_range(&w[1]) {
                        continue;
                    }
                    field.push(
                        [w[0].x * scale, w[0].y * scale],
                        [w[1].x * scale, w[1].y * scale],
                        half,
                    );
                }
            }
        }
        field
    }

    /// Push one whole way through the [`WayFilter`], the way
    /// [`Self::build_with`] does — so a test can drive the real filter without
    /// synthesising an MVT body.
    #[cfg(test)]
    pub(crate) fn push_line(&mut self, filter: WayFilter, kind: &str, points: &[[f32; 2]]) {
        if !filter.admits(kind) {
            return;
        }
        // The same width the tile-side path resolves, from the kind alone — a
        // synthesised way carries no flags. `All` reads no kind, here as there.
        let half = match filter {
            WayFilter::All => None,
            WayFilter::ExcludingRail => crate::vector_tiles::drawn_half_width(kind),
        };
        for pair in points.windows(2) {
            self.push(pair[0], pair[1], half);
        }
    }

    fn push(&mut self, a: [f32; 2], b: [f32; 2], half: Option<f32>) {
        if (b[0] - a[0]).abs() < 1e-4 && (b[1] - a[1]).abs() < 1e-4 {
            return;
        }
        let idx = self.segs.len() as u32;
        self.segs.push((a, b));
        self.halves.push(half);
        let (minx, maxx) = (a[0].min(b[0]), a[0].max(b[0]));
        let (minz, maxz) = (a[1].min(b[1]), a[1].max(b[1]));
        // Clamped to the tile's own grid, which is the backstop for the window
        // above: `as i32` saturates on a float, so an unclamped bound is
        // `i32::MAX` and the loop never finishes.
        for i in bucket_range(minx, maxx, Self::CELL, self.tile_m) {
            for j in bucket_range(minz, maxz, Self::CELL, self.tile_m) {
                self.buckets.entry((i, j)).or_default().push(idx);
            }
        }
    }

    /// The yaw that turns an object at `(x, z)` to face the nearest way within
    /// `max_r` metres — its front (+z) toward the way, its long axis (+x) along it.
    /// `None` if nothing is in range.
    ///
    /// Facing is a **quarter turn** off the way's own direction, which is the part
    /// that is easy to get wrong: a bench lined up *with* the street direction has
    /// its long axis pointing at the kerb. And the quarter turn has a side to it,
    /// so the seat looks at the path rather than at the wall behind it — hence the
    /// perpendicular is signed by which half-plane the object sits in.
    ///
    /// The search widens by whole grid cells until it has covered `max_r`, so a
    /// radius larger than [`Self::CELL`] still finds everything — a bench 20 m off
    /// a road is nearly a full cell away.
    fn facing(&self, x: f32, z: f32, max_r: f32) -> Option<f32> {
        let (a, b) = self.segs[self.nearest(x, z, max_r)?];
        let (wx, wz) = (b[0] - a[0], b[1] - a[1]);
        // Which side of the way the object is on: the sign of the 2D cross product
        // of the way with the offset from it. Zero means the object sits *on* the
        // line, where "toward the way" has no direction — take the left-hand
        // perpendicular rather than collapsing the rotation to nothing.
        let (ox, oz) = (x - a[0], z - a[1]);
        let side = if wx * oz - wz * ox >= 0.0 { 1.0 } else { -1.0 };
        // Right-hand perpendicular of the way, signed to point back at it.
        let (fx, fz) = (side * wz, side * -wx);
        // atan2(x, z) and not (z, x): a yaw of 0 in the object frame points along
        // +z, so this is the rotation that takes +z onto the facing direction.
        Some(fx.atan2(fz))
    }

    /// The yaw that lays an object's local **+z along** the nearest way within
    /// `max_r` metres, rather than across it, and the half-carriageway of that
    /// same way. `None` if nothing is in range.
    ///
    /// A quarter turn from [`Self::facing`], and the difference is the whole
    /// point of having both: a bench turns to *look at* the path, and a level
    /// crossing's masts stand on either side of the road *along* it. The sign is
    /// unresolved — a way has a digitising direction and a road has no front —
    /// so this may answer either of the two opposite yaws. Every form built on it
    /// must therefore be symmetric under a half turn, which the crossings are.
    ///
    /// One lookup answering both is deliberate: a level crossing is oriented
    /// against a road and then has to be *sized* for it, and asking twice could
    /// pick two different roads. The width is `None` whenever the field does not
    /// know it — see [`WayFilter::half_width`] — and every caller must have a
    /// fallback for that.
    pub(crate) fn direction_and_width(
        &self,
        x: f32,
        z: f32,
        max_r: f32,
    ) -> Option<(f32, Option<f32>)> {
        let id = self.nearest(x, z, max_r)?;
        let (a, b) = self.segs[id];
        let (wx, wz) = (b[0] - a[0], b[1] - a[1]);
        Some((wx.atan2(wz), self.halves[id]))
    }

    /// The index of the nearest admitted segment within `max_r`.
    ///
    /// An index rather than the endpoints, because [`Self::halves`] is parallel
    /// to [`Self::segs`] and a caller may want either or both.
    ///
    /// The search widens by whole grid cells until it has covered `max_r`, so a
    /// radius larger than [`Self::CELL`] still finds everything.
    fn nearest(&self, x: f32, z: f32, max_r: f32) -> Option<usize> {
        let reach = (max_r / Self::CELL).ceil() as i32;
        let (cx, cz) = (
            (x / Self::CELL).floor() as i32,
            (z / Self::CELL).floor() as i32,
        );
        let mut best: Option<(f32, usize)> = None;
        for i in cx - reach..=cx + reach {
            for j in cz - reach..=cz + reach {
                let Some(ids) = self.buckets.get(&(i, j)) else {
                    continue;
                };
                for &id in ids {
                    let (a, b) = self.segs[id as usize];
                    let d2 = point_seg_dist2(x, z, a, b);
                    if best.is_none_or(|(bd2, _)| d2 < bd2) {
                        best = Some((d2, id as usize));
                    }
                }
            }
        }
        let (d2, id) = best?;
        (d2 <= max_r * max_r).then_some(id)
    }
}

pub fn point_seg_dist2(x: f32, z: f32, a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dz * dz;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((x - a[0]) * dx + (z - a[1]) * dz) / len2).clamp(0.0, 1.0)
    };
    let (px, pz) = (a[0] + dx * t, a[1] + dz * t);
    (x - px) * (x - px) + (z - pz) * (z - pz)
}

// ─── Geometry ────────────────────────────────────────────────────────────────

/// A material tone. Kept to a handful, because the palette is the point: the map
/// is pale paper and every one of these is a muted step off it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Timber: slats, table tops, swing seats.
    Wood,
    /// Painted steel: posts, frames, bins.
    Metal,
    /// The lighter tone of a roof panel or a sign face, so a shelter's roof reads
    /// against its own posts from directly above — which is the angle the orbit
    /// camera spends most of its time at.
    Roof,
    /// Masonry: a fountain rim, an artwork plinth.
    Stone,
    /// Standing water in a basin. The one saturated tone here, and it is
    /// [`palette::WATER`] — the same colour the canals are, so a fountain is
    /// recognisably the same substance.
    Water,
    /// Glazing — a car's cabin.
    Glass,
    /// Rubber — a tyre.
    Tyre,
    /// Foliage — a hedge. [`palette::CANOPY`], so a clipped hedge is recognisably
    /// the same substance as the tree crowns above it rather than a second, near
    /// green nobody chose.
    Foliage,
    /// **The instance's own** [`FurnitureInstance::paint`] rather than a tone of
    /// this table's. The one part of the scheme that varies within a form, and the
    /// reason is in that field's docs.
    Paint,
}

impl Tone {
    fn rgb(self) -> u32 {
        match self {
            Self::Wood => palette::FURNITURE_WOOD,
            Self::Metal => palette::FURNITURE_METAL,
            Self::Roof => palette::FURNITURE_ROOF,
            Self::Stone => palette::FURNITURE_STONE,
            Self::Water => palette::WATER,
            Self::Glass => palette::VEHICLE_GLASS,
            Self::Tyre => palette::VEHICLE_TYRE,
            Self::Foliage => palette::CANOPY,
            // Answered per instance by [`paint_multiplier`]; this is the no-tint
            // value, so a form using `Paint` without one still draws.
            Self::Paint => palette::FURNITURE,
        }
    }

    /// The vertex-colour multiplier that turns the group's shared base colour into
    /// this tone, computed in **linear** space because that is where the shader
    /// multiplies. Same mechanism as the canopy's two tones: one material for the
    /// whole layer, the variety carried by the mesh.
    fn multiplier(self) -> [f32; 3] {
        paint_multiplier(self.rgb())
    }
}

/// The vertex-colour multiplier that turns [`palette::FURNITURE`] into `rgb`.
///
/// Split out of [`Tone::multiplier`] because [`Tone::Paint`] answers from the
/// instance rather than from the table, and both need the same conversion.
/// `pub(crate)` for [`crate::nav_marks`], which paints from a register's own
/// colour vocabulary rather than from [`Tone`] and so cannot go through the
/// table.
pub(crate) fn paint_multiplier(rgb: u32) -> [f32; 3] {
    let base = srgb_to_linear_rgb(palette::FURNITURE);
    let want = srgb_to_linear_rgb(rgb);
    [
        want[0] / base[0].max(1e-4),
        want[1] / base[1].max(1e-4),
        want[2] / base[2].max(1e-4),
    ]
}

/// One box of one object, in the object's own frame: +x right, +y up, +z **front**
/// (the side that turns toward the path).
pub struct Part {
    /// Centre of the box.
    pub(crate) at: [f32; 3],
    /// Half-extents.
    pub(crate) half: [f32; 3],
    pub(crate) tone: Tone,
}

pub(crate) const fn part(at: [f32; 3], half: [f32; 3], tone: Tone) -> Part {
    Part { at, half, tone }
}

/// The parts of each form. Metres, and measured rather than stylised: a bench seat
/// really is 45 cm off the ground, and getting that wrong is what makes a scene
/// read as a doll's house.
///
/// Named consts rather than array literals in the match arms — a `const fn` call
/// is not implicitly promoted to `'static`, so the literals would be temporaries
/// rebuilt on every lookup, which is once per object per cell.
mod form {
    use super::{part, Part, Tone::*};

    pub const BENCH: [Part; 4] = [
        part([0.0, 0.45, 0.0], [0.90, 0.03, 0.22], Wood),
        part([0.0, 0.68, -0.20], [0.90, 0.20, 0.03], Wood),
        part([-0.75, 0.225, 0.0], [0.05, 0.225, 0.20], Metal),
        part([0.75, 0.225, 0.0], [0.05, 0.225, 0.20], Metal),
    ];
    pub const BIN: [Part; 2] = [
        part([0.0, 0.40, 0.0], [0.18, 0.40, 0.18], Metal),
        part([0.0, 0.84, 0.0], [0.21, 0.04, 0.21], Roof),
    ];
    /// The body is `Paint` rather than `Metal` because a bottle bank's colour is
    /// what says *what goes in it*, and a register that knows the fraction can
    /// say so — see [`super::default_paint`], which keeps an instance that does
    /// not know looking exactly like the painted steel it was.
    pub const RECYCLING: [Part; 2] = [
        part([0.0, 0.70, 0.0], [0.75, 0.70, 0.65], Paint),
        part([0.0, 1.44, 0.0], [0.78, 0.05, 0.68], Roof),
    ];
    pub const SHELTER: [Part; 7] = [
        part([0.0, 2.36, 0.0], [1.70, 0.06, 0.90], Roof),
        part([0.0, 1.25, -0.80], [1.60, 0.95, 0.04], Metal),
        part([-1.55, 1.15, -0.75], [0.06, 1.15, 0.06], Metal),
        part([1.55, 1.15, -0.75], [0.06, 1.15, 0.06], Metal),
        part([-1.55, 1.15, 0.75], [0.06, 1.15, 0.06], Metal),
        part([1.55, 1.15, 0.75], [0.06, 1.15, 0.06], Metal),
        part([0.0, 0.42, -0.50], [1.40, 0.03, 0.20], Wood),
    ];
    pub const PICNIC_TABLE: [Part; 5] = [
        part([0.0, 0.74, 0.0], [0.90, 0.03, 0.35], Wood),
        part([0.0, 0.45, -0.62], [0.90, 0.03, 0.15], Wood),
        part([0.0, 0.45, 0.62], [0.90, 0.03, 0.15], Wood),
        part([-0.75, 0.37, 0.0], [0.04, 0.37, 0.62], Wood),
        part([0.75, 0.37, 0.0], [0.04, 0.37, 0.62], Wood),
    ];
    pub const CABINET: [Part; 3] = [
        part([0.0, 0.20, 0.0], [0.08, 0.20, 0.08], Metal),
        part([0.0, 0.85, 0.0], [0.25, 0.45, 0.20], Metal),
        part([0.0, 1.33, 0.0], [0.27, 0.03, 0.22], Roof),
    ];
    pub const BOARD: [Part; 3] = [
        part([-0.50, 0.50, 0.0], [0.04, 0.50, 0.04], Metal),
        part([0.50, 0.50, 0.0], [0.04, 0.50, 0.04], Metal),
        part([0.0, 1.45, 0.0], [0.60, 0.45, 0.04], Roof),
    ];
    /// A jet in open water: no rim, no basin, no kerb — the pond is the basin.
    ///
    /// A short submerged plinth for the nozzle to stand on and a column of water
    /// tapering upward. It is a still column because the whole furniture group is
    /// one merged mesh with one shared material and nothing in it animates (see
    /// the module note); a plume that does move is `systems::particles`, and a
    /// fountain is a fine thing to hang one on later.
    pub const FOUNTAIN_JET: [Part; 4] = [
        part([0.0, 0.10, 0.0], [0.45, 0.10, 0.45], Stone),
        part([0.0, 0.55, 0.0], [0.22, 0.45, 0.22], Water),
        part([0.0, 1.50, 0.0], [0.13, 0.55, 0.13], Water),
        part([0.0, 2.35, 0.0], [0.07, 0.35, 0.07], Water),
    ];
    pub const FOUNTAIN: [Part; 6] = [
        part([0.0, 0.20, -1.50], [1.60, 0.20, 0.10], Stone),
        part([0.0, 0.20, 1.50], [1.60, 0.20, 0.10], Stone),
        part([-1.50, 0.20, 0.0], [0.10, 0.20, 1.40], Stone),
        part([1.50, 0.20, 0.0], [0.10, 0.20, 1.40], Stone),
        part([0.0, 0.26, 0.0], [1.40, 0.02, 1.40], Water),
        part([0.0, 0.55, 0.0], [0.15, 0.55, 0.15], Stone),
    ];
    /// A 4 m column with the head on a short bracket over the carriageway. The
    /// bracket is what makes it read as a street light rather than as a post from
    /// any distance, and it is why the form takes its angle from the path.
    pub const LAMP: [Part; 3] = [
        part([0.0, 2.00, 0.0], [0.06, 2.00, 0.06], Metal),
        part([0.0, 3.98, 0.28], [0.05, 0.04, 0.28], Metal),
        part([0.0, 3.90, 0.52], [0.14, 0.06, 0.22], Roof),
    ];
    /// A 2 m street rack of the common Dutch kind: two rails a wheel's width apart
    /// at front-wheel height, on a post at each end. Long axis on x, like the
    /// bench, so it squares up along the pavement.
    pub const BIKE_RACK: [Part; 4] = [
        part([0.0, 0.18, 0.0], [1.00, 0.02, 0.02], Metal),
        part([0.0, 0.42, 0.0], [1.00, 0.02, 0.02], Metal),
        part([-0.98, 0.22, 0.0], [0.03, 0.22, 0.12], Metal),
        part([0.98, 0.22, 0.0], [0.03, 0.22, 0.12], Metal),
    ];
    /// A street toilet cabin, 2.3 m tall with a lighter roof and the door on +z.
    pub const TOILET: [Part; 3] = [
        part([0.0, 1.15, 0.0], [0.80, 1.15, 0.80], Metal),
        part([0.0, 2.36, 0.0], [0.88, 0.06, 0.88], Roof),
        part([0.0, 1.00, 0.81], [0.40, 0.95, 0.02], Wood),
    ];
    /// A signal post: a 3 m pole and the head on it, dark, facing the road.
    pub const TRAFFIC_LIGHT: [Part; 2] = [
        part([0.0, 1.50, 0.0], [0.05, 1.50, 0.05], Metal),
        part([0.0, 2.75, 0.12], [0.14, 0.45, 0.10], Tyre),
    ];
    pub const ARTWORK: [Part; 2] = [
        part([0.0, 0.35, 0.0], [0.45, 0.35, 0.45], Stone),
        part([0.0, 1.10, 0.0], [0.25, 0.40, 0.25], Metal),
    ];
    pub const SWING: [Part; 7] = [
        part([-1.30, 1.10, -0.70], [0.05, 1.10, 0.05], Metal),
        part([-1.30, 1.10, 0.70], [0.05, 1.10, 0.05], Metal),
        part([1.30, 1.10, -0.70], [0.05, 1.10, 0.05], Metal),
        part([1.30, 1.10, 0.70], [0.05, 1.10, 0.05], Metal),
        part([0.0, 2.22, 0.0], [1.40, 0.06, 0.06], Metal),
        part([-0.60, 0.50, 0.0], [0.22, 0.03, 0.12], Wood),
        part([0.60, 0.50, 0.0], [0.22, 0.03, 0.12], Wood),
    ];
    /// A hatchback, measured: 4.40 m long, 1.72 m wide, 1.45 m to the roof. Seven
    /// boxes is the whole budget — a car park holds hundreds of these and every
    /// extra box is 24 vertices multiplied by the whole lot (see
    /// [`crate::parking`] on why the count is what bounds this form).
    ///
    /// The body sits 27 cm off the ground and the wheels reach under it, which is
    /// the one detail that stops a stack of boxes reading as a crate.
    ///
    /// **The roof is its own `Paint` box on top of the glass, and the seventh box
    /// buys exactly that.** With the greenhouse as the topmost part every car was
    /// a dark grey rectangle from above — which is where this camera spends most
    /// of its time — so a lot read as a field of identical slabs and the whole
    /// per-instance body colour showed only as a thin skirt below the windows.
    /// Glazing you can see through the *side* of is what a car looks like; a
    /// tinted sunroof over the entire cabin is not.
    pub const CAR: [Part; 7] = [
        part([0.0, 0.60, 0.0], [0.86, 0.33, 2.20], Paint),
        part([0.0, 1.14, -0.15], [0.80, 0.22, 1.15], Glass),
        part([0.0, 1.40, -0.15], [0.79, 0.05, 1.12], Paint),
        part([-0.86, 0.32, 1.45], [0.10, 0.32, 0.32], Tyre),
        part([0.86, 0.32, 1.45], [0.10, 0.32, 0.32], Tyre),
        part([-0.86, 0.32, -1.45], [0.10, 0.32, 0.32], Tyre),
        part([0.86, 0.32, -1.45], [0.10, 0.32, 0.32], Tyre),
    ];
    /// A city bike: 1.75 m long, 1.03 m to the bars. Four boxes, and the wheels
    /// are 6 cm slabs rather than discs — from the side that is a wheel, and from
    /// any other angle a bike is a thicket of other bikes anyway.
    pub const BICYCLE: [Part; 4] = [
        part([0.0, 0.34, 0.52], [0.03, 0.34, 0.34], Tyre),
        part([0.0, 0.34, -0.52], [0.03, 0.34, 0.34], Tyre),
        part([0.0, 0.62, 0.0], [0.03, 0.16, 0.50], Paint),
        part([0.0, 1.00, 0.40], [0.24, 0.03, 0.03], Metal),
    ];
    /// A person: 1.72 m tall, shoulders at 1.4 m.
    ///
    /// Three boxes, and the head is [`Tone::Metal`] rather than a skin colour on
    /// purpose — a crowd rendered in any one complexion is a claim about a city's
    /// people that a road centreline cannot support, and six would be a worse one.
    /// It reads as a figure, not as a person you could name, which is the honest
    /// resolution for something inferred from "a pavement exists here".
    ///
    /// The legs are a single box with the gap left in front-to-back rather than
    /// two: at this size two would alias into one anyway, and the silhouette that
    /// matters is *upright and narrow*, which is what stops it reading as a
    /// bollard.
    pub const PERSON: [Part; 3] = [
        part([0.0, 0.42, 0.0], [0.17, 0.42, 0.11], Paint),
        part([0.0, 1.10, 0.0], [0.22, 0.28, 0.14], Paint),
        part([0.0, 1.52, 0.0], [0.10, 0.12, 0.10], Metal),
    ];
    /// A canal boat: 9 m long, 2.4 m in the beam, a low cabin aft of centre.
    /// Measured off the small *woonboten* and sloops a Groningen canal is lined
    /// with, not off a barge — a barge on a 12 m canal reads as a ship in a
    /// bathtub.
    ///
    /// The hull straddles y = 0 because that is where the water surface is: the
    /// bottom 20 cm is under it and the rest is freeboard. It is the one form
    /// here with anything below the ground plane, and it is deliberate.
    pub const BOAT: [Part; 3] = [
        part([0.0, 0.10, 0.0], [1.20, 0.30, 4.50], Paint),
        part([0.0, 0.46, -0.90], [0.95, 0.06, 2.60], Roof),
        part([0.0, 0.78, -1.30], [0.80, 0.32, 1.60], Metal),
    ];
    /// A 10 m sailing yacht: a painted (usually white) hull, a pale cabin aft of
    /// the mast and the mast itself, which is what reads as "marina" from any
    /// height — a forest of masts over the jetties.
    pub const YACHT: [Part; 4] = [
        part([0.0, 0.15, 0.0], [1.60, 0.45, 5.00], Paint),
        // Deck and cabin in the hull's own paint: from the air a yacht is its
        // deck, and a beige deck on a white hull read as a jetty plank.
        part([0.0, 0.62, 3.80], [0.90, 0.03, 1.20], Paint),
        part([0.0, 0.95, -0.60], [1.05, 0.38, 2.20], Paint),
        part([0.0, 6.20, 1.00], [0.06, 5.60, 0.06], Metal),
    ];
    /// A Dutch houseboat (`woonschip`): a long, low hull and a house on it.
    pub const HOUSEBOAT: [Part; 3] = [
        part([0.0, 0.10, 0.0], [2.50, 0.45, 9.00], Paint),
        part([0.0, 1.85, -0.50], [2.20, 1.30, 7.00], Wood),
        // Grey: the roofs the photograph shows are steel and bitumen.
        part([0.0, 3.20, -0.50], [2.40, 0.08, 7.30], Metal),
    ];
}

pub fn parts(kind: FurnitureKind) -> &'static [Part] {
    match kind {
        FurnitureKind::Bench => &form::BENCH,
        FurnitureKind::Bin => &form::BIN,
        FurnitureKind::Recycling => &form::RECYCLING,
        FurnitureKind::Shelter => &form::SHELTER,
        FurnitureKind::PicnicTable => &form::PICNIC_TABLE,
        FurnitureKind::Cabinet => &form::CABINET,
        FurnitureKind::Board => &form::BOARD,
        FurnitureKind::Fountain => &form::FOUNTAIN,
        FurnitureKind::Artwork => &form::ARTWORK,
        FurnitureKind::Swing => &form::SWING,
        FurnitureKind::Lamp => &form::LAMP,
        FurnitureKind::Car => &form::CAR,
        FurnitureKind::Bicycle => &form::BICYCLE,
        FurnitureKind::Person => &form::PERSON,
        FurnitureKind::Boat => &form::BOAT,
        FurnitureKind::TrafficLight => &form::TRAFFIC_LIGHT,
        FurnitureKind::Yacht => &form::YACHT,
        FurnitureKind::Houseboat => &form::HOUSEBOAT,
        FurnitureKind::BikeRack => &form::BIKE_RACK,
        FurnitureKind::Toilet => &form::TOILET,
    }
}

impl FurnitureInstance {
    /// Whether an operator's placeholder model may stand in for this instance.
    ///
    /// True for everything except a fountain in open water. A model hung on the
    /// `fountain` slug is a thing that stands on the ground — it has a base, it is
    /// placed with a yaw, and an operator uploading one is picturing a square.
    /// Dropping it 40 m out into a pond is the plaza basin at the waterline all
    /// over again, arriving through the model exchange instead of through
    /// [`form::FOUNTAIN`].
    ///
    /// The cost is stated plainly: there is no way to give the jets a model of
    /// their own. That is the conservative half of the trade, and it is the right
    /// way round — an operator who sets a fountain model and finds one lake
    /// unchanged has a missing feature, while one who finds a stone basin floating
    /// in a lake has the bug this flag exists to remove.
    #[must_use]
    pub fn takes_placeholder_model(&self) -> bool {
        !(self.over_water && matches!(self.kind, FurnitureKind::Fountain))
    }
}

/// The form one *instance* is drawn as.
///
/// [`parts`] answers for a kind; this answers for the object, and the two differ
/// only where the same surveyed kind is genuinely two things on the ground. Use
/// this at every mesh site — a caller that reaches for `parts` directly gets the
/// plaza basin in the middle of a pond.
#[must_use]
pub fn parts_for(item: &FurnitureInstance) -> &'static [Part] {
    if item.over_water && matches!(item.kind, FurnitureKind::Fountain) {
        return &form::FOUNTAIN_JET;
    }
    // The hull is the form's first part (`form::HOUSEBOAT`).
    if item.hull_only && matches!(item.kind, FurnitureKind::Houseboat) {
        return &form::HOUSEBOAT[..1];
    }
    parts(item.kind)
}

/// How far apart (m) an OSM object and a surveyed one of the same form may be and
/// still be the same object. Both sources are mapped against aerial photography,
/// which puts one bench a metre or two from itself; two real benches of a row
/// stand further apart than this.
const SURVEYED_TWIN_M: f32 = 2.5;

/// Drop each `pois` instance that a surveyed one of the same form already stands
/// on. The survey is the one kept: it is the register, and OSM's copy of the
/// object was traced from the same street.
pub fn drop_surveyed_twins(pois: &mut Vec<FurnitureInstance>, surveyed: &[FurnitureInstance]) {
    if surveyed.is_empty() {
        return;
    }
    let r2 = SURVEYED_TWIN_M * SURVEYED_TWIN_M;
    pois.retain(|p| {
        !surveyed
            .iter()
            .any(|s| s.kind == p.kind && (s.x - p.x).powi(2) + (s.z - p.z).powi(2) <= r2)
    });
}

/// Turn **surveyed** points into oriented furniture instances.
///
/// The companion to [`extract_furniture`], for objects that come from a source
/// this module does not read. The caller supplies tile-local metres and a form —
/// `systems::surveyed` gets both from the BGT — and everything after that is the
/// same: the same hashed luma so a street is not one flat grey, and the same
/// [`WayField`] so a lamp's bracket reaches over the carriageway instead of
/// pointing at a hashed angle.
///
/// It takes the tile for the way field alone. That is the whole reason this lives
/// here rather than in the client: the field is built from the *`streets`* layer
/// of the very tile the cell is being meshed from, and exposing it would mean
/// exporting the field, its scale rule and the trap in its doc comment.
#[must_use]
pub fn place_surveyed(
    tile: &TileData,
    tile_m: f32,
    seed: u64,
    points: &[(f32, f32, FurnitureKind, Option<u32>)],
) -> Vec<FurnitureInstance> {
    if points.is_empty() {
        return Vec::new();
    }
    let ways = WayField::build(tile, tile_m);
    let mut out = Vec::with_capacity(points.len());
    for (i, &(x, z, kind, paint)) in points.iter().enumerate() {
        let h = hash3(seed, i as i32, kind as i32);
        let yaw = kind
            .takes_angle_from_path()
            .then(|| ways.facing(x, z, ORIENT_RADIUS_M))
            .flatten()
            .unwrap_or_else(|| rand01(h, 0) * std::f32::consts::TAU);
        // Same question as in `extract_furniture`, and it has to be asked here too:
        // the municipal register is where "Fontein <square>" comes from, and
        // that one stands in a pond.
        let over_water =
            matches!(kind, FurnitureKind::Fountain) && water_under(tile, x / tile_m, z / tile_m);
        out.push(FurnitureInstance {
            x,
            z,
            yaw,
            kind,
            luma: 1.0 + (rand01(h, 1) - 0.5) * 0.10,
            paint: paint.unwrap_or_else(|| default_paint(kind)),
            over_water,
            hull_only: false,
        });
    }
    out
}

/// The body colour a form is drawn in when nothing has said otherwise.
///
/// Only [`Tone::Paint`] parts read it, so for every other form this is the
/// no-tint value it always was. [`FurnitureKind::Recycling`] is the exception:
/// its body became `Paint` so a register that knows the fraction can colour it,
/// and this is the painted steel it looked like before that — an unpainted bottle
/// bank must not change appearance just because it *could* now be coloured.
#[must_use]
pub fn default_paint(kind: FurnitureKind) -> u32 {
    match kind {
        FurnitureKind::Recycling => palette::FURNITURE_METAL,
        _ => palette::FURNITURE,
    }
}

/// Merge a cell's furniture into one group's meshes, each no larger than
/// `max_bytes`.
///
/// One group, one material — the reason a street's worth of objects costs a
/// handful of draw calls rather than a thousand.
///
/// It used to be exactly one mesh per cell, and that was safe only while the
/// cell's whole vocabulary was surveyed points: 467 of them in the Groningen
/// fixture, five boxes each. [`crate::parking`] feeds the same group from an
/// *area* — a car park is filled, not surveyed, so the count follows the ground
/// it covers — and that is how the canopies came to be the largest asset in the
/// app (`build_vegetation_meshes` has the measurement). So the cut is the same
/// cut, for the same reason, taken **between** objects so no car loses its wheels.
///
/// `max_bytes` is `None` on every rung without an upload ceiling, which restores
/// the single-mesh behaviour exactly.
/// [`build_furniture_meshes`] without the forms in `skip`.
///
/// A form the server draws with an uploaded model of its own
/// (`systems::placeholder_models`) must not also be built out of boxes here. It
/// is filtered at the source rather than hidden afterwards: hiding costs the
/// extraction, the merge and the upload for geometry nobody sees.
///
/// A copy of the surviving instances is made only when there is something to
/// skip, so the ordinary case — no placeholders anywhere — allocates nothing.
#[must_use]
pub fn build_furniture_meshes_except(
    items: &[FurnitureInstance],
    skip: FurnitureMask,
    max_bytes: Option<usize>,
) -> Vec<SurfaceMesh> {
    if skip.is_empty() {
        return build_furniture_meshes(items, max_bytes);
    }
    // An instance the mask covers is left *unmeshed* because a model will stand
    // where it was — so one that refuses a model has to keep its geometry, or it
    // disappears from the map entirely the moment an operator sets a fountain.
    let kept: Vec<FurnitureInstance> = items
        .iter()
        .filter(|item| !skip.contains(item.kind) || !item.takes_placeholder_model())
        .copied()
        .collect();
    build_furniture_meshes(&kept, max_bytes)
}

/// [`build_furniture_meshes_except`] with every instance raised by `lift(item)`,
/// one height for the whole object.
///
/// For the things that do not stand on the ground. A moored boat floats: its
/// hull is level at the water's surface whatever the bank beside it does, and
/// draped vertex by vertex on the ground like a bench it was tipped up the slope
/// of a pond's bank and stood at quay level in a canal (the study pond,
/// 2026-10-01). The caller places these meshes itself rather than handing them
/// to the terrain drape.
#[must_use]
pub fn build_furniture_meshes_lifted(
    items: &[FurnitureInstance],
    skip: FurnitureMask,
    max_bytes: Option<usize>,
    lift: &dyn Fn(&FurnitureInstance) -> f32,
) -> Vec<SurfaceMesh> {
    let kept: Vec<FurnitureInstance> = items
        .iter()
        .filter(|item| !skip.contains(item.kind) || !item.takes_placeholder_model())
        .copied()
        .collect();
    build_furniture_meshes_with(&kept, max_bytes, lift)
}

#[must_use]
pub fn build_furniture_meshes(
    items: &[FurnitureInstance],
    max_bytes: Option<usize>,
) -> Vec<SurfaceMesh> {
    build_furniture_meshes_with(items, max_bytes, &|_| 0.0)
}

fn build_furniture_meshes_with(
    items: &[FurnitureInstance],
    max_bytes: Option<usize>,
    lift: &dyn Fn(&FurnitureInstance) -> f32,
) -> Vec<SurfaceMesh> {
    if items.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = MeshBuf::default();
    // In curve order, so each piece is one patch of the cell and the culling can drop
    // the ones out of view and out of each shadow cascade (`crate::patches`).
    for item in crate::patches::spatial_order(items, |i| [i.x, i.z])
        .into_iter()
        .map(|i| &items[i])
    {
        // Flush *before* adding, and only with something to flush — a piece must
        // hold at least one object or a cap under one form's cost would emit
        // empty meshes forever.
        if let Some(max) = max_bytes {
            let step = item_bytes(item);
            if buf.bytes() + step > max && !buf.positions.is_empty() {
                out.push(std::mem::take(&mut buf).finish(SurfaceGroup::Furniture));
            }
        }
        let (sn, cs) = item.yaw.sin_cos();
        let up = lift(item);
        if item.kind == FurnitureKind::Car {
            push_parked_car(&mut buf, item, (sn, cs), up);
            continue;
        }
        for p in parts_for(item) {
            let m = match p.tone {
                Tone::Paint => paint_multiplier(item.paint),
                tone => tone.multiplier(),
            };
            let tint = [m[0] * item.luma, m[1] * item.luma, m[2] * item.luma, 1.0];
            let raised;
            let part = if up == 0.0 {
                p
            } else {
                raised = Part {
                    at: [p.at[0], p.at[1] + up, p.at[2]],
                    ..*p
                };
                &raised
            };
            push_box(&mut buf, item.x, item.z, (sn, cs), part, tint);
        }
    }
    if !buf.positions.is_empty() {
        out.push(buf.finish(SurfaceGroup::Furniture));
    }
    out
}

/// Which of the moving traffic's bodies a parked car at this spot is.
///
/// Hashed off the bay's own position, so a lot that streams out and back is
/// parked with the same cars — the same reason its vacancies are hashed.
#[must_use]
pub fn parked_body(item: &FurnitureInstance) -> crate::agent_models::CarBody {
    let mut h = u64::from(item.x.to_bits()).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ u64::from(item.z.to_bits()).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 31;
    crate::agent_models::CarBody::for_index((h % 10_007) as usize)
}

/// What one instance adds to a furniture mesh, in upload bytes.
#[must_use]
pub fn item_bytes(item: &FurnitureInstance) -> usize {
    if item.kind == FurnitureKind::Car {
        let m = parked_body(item).parked();
        let (v, i) = [&m.body, &m.trim, &m.tail]
            .iter()
            .fold((0, 0), |(v, i), p| {
                (v + p.positions.len(), i + p.indices.len())
            });
        return v * VERTEX_BYTES + i * INDEX_BYTES;
    }
    parts_for(item).len() * BOX_BYTES
}

/// A parked car: one of the moving traffic's bodies (`agent_models`), at its
/// parked level of detail, so a car at the kerb and one driving past it are the
/// same car again. [`form::CAR`] stays for what still asks for its boxes.
///
/// Merged like every other form, so the paint is a vertex colour here: the body
/// carries the instance's paint and the trim its own colours, each as the
/// multiplier over the furniture material's base that every tone already is.
fn push_parked_car(buf: &mut MeshBuf, item: &FurnitureInstance, (sn, cs): (f32, f32), up: f32) {
    let m = parked_body(item).parked();
    let base = srgb_to_linear_rgb(palette::FURNITURE);
    let paint = paint_multiplier(item.paint);
    let l = item.luma;
    for (mesh, painted) in [(&m.body, true), (&m.trim, false), (&m.tail, false)] {
        let start = buf.positions.len() as u32;
        for (k, p) in mesh.positions.iter().enumerate() {
            let [wx, wz] = place_ground(item.x, item.z, (sn, cs), p[0], p[2]);
            buf.positions.push([wx, p[1] + up, wz]);
            let n = mesh.normals[k];
            buf.normals
                .push([n[0] * cs + n[2] * sn, n[1], -n[0] * sn + n[2] * cs]);
            let c = mesh.colors[k];
            buf.colors.push(if painted {
                [paint[0] * l, paint[1] * l, paint[2] * l, 1.0]
            } else {
                [
                    c[0] / base[0].max(1e-4) * l,
                    c[1] / base[1].max(1e-4) * l,
                    c[2] / base[2].max(1e-4) * l,
                    1.0,
                ]
            });
        }
        buf.indices.extend(mesh.indices.iter().map(|i| i + start));
    }
}

/// A surveyed boundary: a fence, a wall or a hedge, as the line it was surveyed
/// as.
///
/// Points are tile-local metres, the frame everything else here works in.
#[derive(Debug, Clone)]
pub struct SurveyedLine {
    pub points: Vec<[f32; 2]>,
    pub kind: LineKind,
    /// Measured height in metres, where the register has one.
    ///
    /// `None` is the ordinary case and means "use the form's nominal height" —
    /// plain BGT is flat geometry and says nothing about how tall anything is. The
    /// measurement comes from the Kadaster's 3D basisvoorziening, whose
    /// `Scheiding_vlak` objects are the same boundaries extruded, joined on the
    /// identificatie in the extractor.
    ///
    /// It matters more than a refinement, because the nominal height is what made
    /// half of this layer undrawable. [`LineKind::Wall`] is a 2.0 m slab, which is
    /// a reasonable garden wall and is nothing like the register's actual
    /// distribution: over one 2 km block of Groningen the median surveyed `muur` is
    /// **0.23 m** and the median `kademuur` is **1.29 m**. Drawing every quay wall
    /// and every bank protection as a uniform 2 m of masonry is why they were
    /// dropped from the extractor in the first place — correctly, for that form.
    /// With a measured height there is nothing left to object to, so this landing
    /// is what lets those types be accepted at all.
    pub height_m: Option<f32>,
}

/// Half the width of a kerbstone, so its box covers the band
/// `paving_edges::KERB_M` wide.
pub const KERB_HALF_M: f32 = crate::paving_edges::KERB_M / 2.0;

/// The three linear forms. Mirrors `LINE_KINDS` in atlas's `pipeline/bgt.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Garden fencing and wire mesh.
    Fence,
    /// Masonry, and the noise barriers along a ring road — both a solid slab
    /// standing on the ground, and telling them apart at this scale would be a
    /// distinction nobody can see.
    Wall,
    /// A clipped hedge.
    Hedge,
    /// A retaining wall — a quay wall, sheet piling, bank protection. Its top
    /// is the ground at its line and its measured height is the face *below*
    /// that, so only the side where the ground or the water drops away shows.
    Quay,
    /// A kerbstone along a surveyed pavement edge (`paving_edges::kerb_lines`).
    /// **Not on the wire**: the BGT has no kerbs, so this is never parsed, only
    /// derived from the boundary between a raised surface and a carriageway.
    Kerb,
}

impl LineKind {
    /// Height, half-thickness and tone, metres.
    ///
    /// Measured against the real things rather than stylised: a garden fence is
    /// waist-to-shoulder, a hedge is trimmed at about the same height and is far
    /// thicker, a noise barrier is well over head height. Getting these wrong is
    /// what makes a street read as a model railway.
    const fn spec(self) -> (f32, f32, Tone) {
        match self {
            Self::Fence => (1.2, 0.03, Tone::Wood),
            // **The nominal wall is low on purpose, and 2.0 m was doing real
            // damage.** This height is only used where the register has no
            // measurement (`SurveyedLine::height_m`), which is most of the linear
            // boundaries — the 3D product publishes polygons only, so every
            // boundary surveyed as a *line* falls back to this number. Over one
            // Groningen cell that is 3,698 m of the 6,264 m drawn.
            //
            // At 2.0 m that is an opaque slab you cannot see over, and along a
            // canal or a pond it walls the water off completely — the thing the
            // extractor's old exclusion of the bank protections was trying to
            // avoid, arriving through the ordinary `muur` instead. The register's
            // own measured distribution says so plainly: median 0.23 m, p90
            // 1.46 m. A real one at the study pond is about sitting height and
            // you look straight over it at the water.
            //
            // 0.9 m sits between that median and its p90. Being too low costs a
            // wall that reads as edging; being too high costs the view, and only
            // one of those is recoverable by looking somewhere else.
            //
            // The honest fix is a measurement per line, and the second-best is a
            // form of its own for the noise barriers, which genuinely are 2–4 m
            // and are 3 features in a city here. Both are follow-ups; neither is a
            // reason to keep walling off every canal in the country.
            Self::Wall => (0.9, 0.09, Tone::Stone),
            Self::Hedge => (1.1, 0.35, Tone::Foliage),
            // Only ever drawn at a measured height (atlas gates the type on one).
            Self::Quay => (0.9, 0.12, Tone::Stone),
            // Standing 10 cm proud of the carriageway, the top face exactly over
            // the flat kerb band `paving_edges` cuts out of the pavement. The
            // pavement itself stays at grade: lifting it would bury the verge's
            // grass and sink every lamppost and bench standing on it.
            Self::Kerb => (0.10, KERB_HALF_M, Tone::Roof),
        }
    }
}

/// Shortest measured boundary worth drawing, metres.
///
/// Under this a [`SurveyedLine::height_m`] is read as "no measurable height"
/// rather than as a very low wall: the 3D product's z is a drape over AHN, so a
/// flat object comes back as a few centimetres of ground noise. It is also the
/// kerb line — a third of the register's `muur` objects are edging, and edging
/// drawn as a slab is a ribbon of z-fight down the pavement.
pub const MIN_MEASURED_LINE_M: f32 = 0.30;

/// Tallest measured boundary this form will draw, metres.
///
/// A ceiling on the *form*, not on the data: past about this the object is a
/// retaining structure holding back a road embankment, and six boxes in a row is
/// the wrong thing to draw for it. The register's tallest quay wall in the
/// Groningen block is 4.45 m, so this clamps almost nothing and is here to stop a
/// bad join putting a building-height slab down a street.
pub const MAX_MEASURED_LINE_M: f32 = 6.0;

/// Shortest segment worth a box, metres. The survey emits vertices wherever it
/// found a change of direction, and a few of those are centimetres apart.
const MIN_SEGMENT_M: f32 = 0.15;

/// Mesh a cell's surveyed boundaries — **one box per surveyed segment**, at that
/// segment's own length.
///
/// Not a resampling. The obvious approach is to walk each line in fixed steps and
/// stand a panel at each, which is what the point forms above would have made
/// easy; it is both dearer and worse. Dearer because the survey's own segments
/// average 6.4 m over Groningen and a 2 m step would triple the box count for the
/// same fence. Worse because a fixed step cannot land on a corner, so every bend
/// in a wall gains a gap on the outside and an overlap on the inside.
///
/// So each segment becomes one box, turned to run along itself. A corner is then
/// two boxes meeting at their ends, which reads correctly from any distance this
/// layer is visible at.
#[must_use]
pub fn build_line_meshes(lines: &[SurveyedLine], max_bytes: Option<usize>) -> Vec<SurfaceMesh> {
    build_line_meshes_footed(lines, max_bytes, &|_, _| 0.0)
}

/// Longest box a measured wall is drawn in, metres. A straight surveyed segment
/// can run fifty metres along a bank whose ground falls away under it; cut into
/// equal pieces it can follow that ground and find its foot along the way, and
/// equal pieces of one straight segment meet end to end, so nothing is lost at
/// a corner.
pub const WALL_PIECE_M: f32 = 6.0;

/// [`build_line_meshes`] with the ground beside each wall taken into account.
///
/// **A measured wall's height is measured from its foot, and its foot is the
/// lower side.** A bank or quay wall holds the walkway up: the register measures
/// the face from the water or the lower terrace, and the top is flush with the
/// ground behind it. Stood on the ground at its own line — which is the upper
/// side — it was drawn that whole height *above* the walkway: two 2 m slabs down
/// the study pond's stepped bank at a nearby square (2026-10-01). `foot(mid,
/// dir)` answers how far the ground on the lower side of a piece of wall lies
/// below the ground at the wall, metres (≥ 0), for the piece centred at `mid`
/// running along unit `dir`, both tile-local; the piece is sunk by that much,
/// never by more than its own height, so a freestanding wall on level ground is
/// unchanged. Only measured walls are asked — a nominal height is a guess about
/// a wall standing on the ground, and has no foot to find.
#[must_use]
pub fn build_line_meshes_footed(
    lines: &[SurveyedLine],
    max_bytes: Option<usize>,
    foot: &dyn Fn([f32; 2], [f32; 2]) -> f32,
) -> Vec<SurfaceMesh> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = MeshBuf::default();
    for line in lines {
        let (nominal, half_thick, tone) = line.kind.spec();
        // A measured height wins, clamped to something a survey could mean. The
        // floor is the important end: 3dfier drapes these objects on AHN, so an
        // object with no real elevation lands as a few centimetres of ground noise,
        // and a 4 cm wall is a strip of z-fighting on the pavement rather than a
        // low wall. Below the floor there is nothing to draw and the line is
        // skipped — which is the honest answer for the third of surveyed `muur`
        // that are kerbs and edging rather than walls.
        let height = match line.height_m {
            Some(m) if m < MIN_MEASURED_LINE_M => continue,
            Some(m) => m.min(MAX_MEASURED_LINE_M),
            None => nominal,
        };
        let tint = {
            let m = tone.multiplier();
            [m[0], m[1], m[2], 1.0]
        };
        for pair in line.points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
            let len = (dx * dx + dz * dz).sqrt();
            if len < MIN_SEGMENT_M {
                continue;
            }
            // `push_box` maps the box's local +x onto world `(cos, -sin)`, so the
            // rotation that lays a box along `(dx, dz)` is this one. Same
            // convention as [`WayField::facing`], which maps local +z instead —
            // the two are a quarter turn apart and both are stated where they are
            // used, because guessing wrong lays every fence across its own line.
            let (ux, uz) = (dx / len, dz / len);
            let (sn, cs) = (-uz, ux);
            // Only a measured wall is cut into pieces and footed; see
            // [`build_line_meshes_footed`].
            let measured = line.height_m.is_some();
            let pieces = if measured {
                (len / WALL_PIECE_M).ceil().max(1.0) as usize
            } else {
                1
            };
            let piece = len / pieces as f32;
            for k in 0..pieces {
                if let Some(max) = max_bytes {
                    if buf.bytes() + BOX_BYTES > max && !buf.positions.is_empty() {
                        out.push(std::mem::take(&mut buf).finish(SurfaceGroup::Furniture));
                    }
                }
                let t = (k as f32 + 0.5) * piece;
                let mid = [a[0] + ux * t, a[1] + uz * t];
                // A retaining wall's top *is* the ground at its line, so it is
                // sunk its whole height; any other measured wall finds its foot.
                let sink = if line.kind == LineKind::Quay {
                    height
                } else if measured {
                    foot(mid, [ux, uz]).clamp(0.0, height)
                } else {
                    0.0
                };
                let part = Part {
                    at: [0.0, height * 0.5 - sink, 0.0],
                    half: [piece * 0.5, height * 0.5, half_thick],
                    tone,
                };
                push_box(&mut buf, mid[0], mid[1], (sn, cs), &part, tint);
            }
        }
    }
    if !buf.positions.is_empty() {
        out.push(buf.finish(SurfaceGroup::Furniture));
    }
    out
}

/// A bridge railing's height over its deck, metres: the Dutch minimum for a
/// parapet beside a footway or cycle path is 1.0 m, and most stand a little over.
pub const RAILING_HEIGHT_M: f32 = 1.1;

/// How far apart a railing's posts stand, metres.
const RAILING_POST_M: f32 = 2.5;

/// Longest rail drawn as one box, metres. A deck that slopes has its meshes cut
/// to its profile's own step after this (`terrain_refine`), so the rail follows
/// it either way; this only keeps a long straight one from being one box whose
/// ends are tens of metres from its middle.
const RAIL_PIECE_M: f32 = 6.0;

/// Mesh a bridge's railings along `runs` (`vector_tiles::railing_runs`), at
/// `y = 0` for the deck's surface: a post every [`RAILING_POST_M`] and at every
/// corner, a handrail along the top and a rail at mid-height, in painted steel.
///
/// Drawn as the posts and rails a railing is, not as a slab, because a slab a
/// metre high down both sides of a canal bridge walls off the water it crosses —
/// which is the view a bridge is for.
#[must_use]
pub fn build_railing_meshes(runs: &[Vec<[f32; 2]>], max_bytes: Option<usize>) -> Vec<SurfaceMesh> {
    let mut out = Vec::new();
    let mut buf = MeshBuf::default();
    let tone = Tone::Metal;
    let tint = {
        let m = tone.multiplier();
        [m[0], m[1], m[2], 1.0]
    };
    let h = RAILING_HEIGHT_M;
    let mut push = |buf: &mut MeshBuf, at: [f32; 2], (sn, cs): (f32, f32), part: Part| {
        if let Some(max) = max_bytes {
            if buf.bytes() + BOX_BYTES > max && !buf.positions.is_empty() {
                out.push(std::mem::take(buf).finish(SurfaceGroup::Furniture));
            }
        }
        push_box(buf, at[0], at[1], (sn, cs), &part, tint);
    };
    for run in runs {
        // Distance walked since the last post, so posts keep their spacing
        // round a corner instead of restarting at every vertex.
        let mut since_post = f32::INFINITY;
        for (k, w) in run.windows(2).enumerate() {
            let (a, b) = (w[0], w[1]);
            let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
            let len = dx.hypot(dz);
            if len < MIN_SEGMENT_M {
                continue;
            }
            let (ux, uz) = (dx / len, dz / len);
            // The rotation that lays a box's local +x along `(dx, dz)`; see
            // `build_line_meshes_footed`.
            let rot = (-uz, ux);
            let pieces = (len / RAIL_PIECE_M).ceil().max(1.0) as usize;
            let piece = len / pieces as f32;
            for p in 0..pieces {
                let t = (p as f32 + 0.5) * piece;
                let mid = [a[0] + ux * t, a[1] + uz * t];
                push(
                    &mut buf,
                    mid,
                    rot,
                    part([0.0, h - 0.04, 0.0], [piece * 0.5, 0.04, 0.05], tone),
                );
                push(
                    &mut buf,
                    mid,
                    rot,
                    part([0.0, h * 0.5, 0.0], [piece * 0.5, 0.025, 0.02], tone),
                );
            }
            // A post at the corner this segment starts from, and along it.
            if k == 0 || since_post >= RAILING_POST_M * 0.5 {
                push(
                    &mut buf,
                    a,
                    rot,
                    part([0.0, h * 0.5, 0.0], [0.04, h * 0.5, 0.04], tone),
                );
                since_post = 0.0;
            }
            let mut d = RAILING_POST_M - since_post;
            while d < len - RAILING_POST_M * 0.5 {
                let at = [a[0] + ux * d, a[1] + uz * d];
                push(
                    &mut buf,
                    at,
                    rot,
                    part([0.0, h * 0.5, 0.0], [0.04, h * 0.5, 0.04], tone),
                );
                d += RAILING_POST_M;
            }
            since_post = len - (d - RAILING_POST_M);
        }
        // …and the one the run ends on.
        if let (Some(&last), Some(&prev)) = (run.last(), run.iter().rev().nth(1)) {
            let (dx, dz) = (last[0] - prev[0], last[1] - prev[1]);
            let len = dx.hypot(dz).max(1e-6);
            push(
                &mut buf,
                last,
                (-dz / len, dx / len),
                part([0.0, h * 0.5, 0.0], [0.04, h * 0.5, 0.04], tone),
            );
        }
    }
    if !buf.positions.is_empty() {
        out.push(buf.finish(SurfaceGroup::Furniture));
    }
    out
}

/// What one [`Part`] costs to upload: [`push_box`] emits 24 vertices and 36
/// indices, flat-shaded, every time.
pub(crate) const BOX_BYTES: usize = 24 * VERTEX_BYTES + 36 * INDEX_BYTES;

/// The accumulator every builder in this group writes into.
///
/// `pub(crate)` so [`crate::nav_marks`] can merge into the same group with the
/// same byte accounting — its forms are round rather than boxes, but a
/// navigation mark and a bench end up in one mesh, one material and one draw
/// call, which is the whole reason the group exists.
#[derive(Default)]
pub(crate) struct MeshBuf {
    pub(crate) positions: Vec<[f32; 3]>,
    pub(crate) normals: Vec<[f32; 3]>,
    pub(crate) colors: Vec<[f32; 4]>,
    pub(crate) indices: Vec<u32>,
}

impl MeshBuf {
    /// What this buffer will cost to upload, in the units the frame budget is in.
    pub(crate) fn bytes(&self) -> usize {
        self.positions.len() * VERTEX_BYTES + self.indices.len() * INDEX_BYTES
    }

    pub(crate) fn finish(self, group: SurfaceGroup) -> SurfaceMesh {
        SurfaceMesh {
            group,
            positions: self.positions,
            indices: self.indices,
            uvs: Vec::new(),
            normals: self.normals,
            colors: self.colors,
            rungs: Vec::new(),
        }
    }
}

/// Object frame → world on the ground plane: rotate `(x, z)` about +y, then
/// translate by the object's own position.
///
/// Split out of [`push_box`] rather than left as its closure because a layer that
/// has to know *where its own boxes landed* — `crossings`, for the collider on a
/// mast — must not carry a second copy of this rotation. A sign error in a copy is
/// invisible in a still frame and lands the collider on the wrong side of the
/// road; this tree has already paid for that mistake twice
/// (`interiors`' door normals).
pub(crate) fn place_ground(ox: f32, oz: f32, (sn, cs): (f32, f32), lx: f32, lz: f32) -> [f32; 2] {
    [ox + lx * cs + lz * sn, oz - lx * sn + lz * cs]
}

/// Emit one yaw-rotated box: 6 flat-shaded faces, 24 vertices, 12 triangles.
///
/// Faces are not shared between vertices — a box shaded off averaged corner
/// normals loses its edges, and a crisp edge is the only thing telling a 40 cm bin
/// apart from a smudge at this size.
pub(crate) fn push_box(
    buf: &mut MeshBuf,
    ox: f32,
    oz: f32,
    (sn, cs): (f32, f32),
    p: &Part,
    tint: [f32; 4],
) {
    // Object frame → world: rotate (x, z) about +y, then translate.
    let place = |lx: f32, ly: f32, lz: f32| {
        let [wx, wz] = place_ground(ox, oz, (sn, cs), lx, lz);
        [wx, ly, wz]
    };
    let rotate = |nx: f32, nz: f32| [nx * cs + nz * sn, 0.0, -nx * sn + nz * cs];

    // (normal, and the four corners of that face as ±1 in the box's own axes)
    const FACES: [([f32; 3], [[f32; 3]; 4]); 6] = [
        // +x
        (
            [1.0, 0.0, 0.0],
            [
                [1.0, -1.0, -1.0],
                [1.0, -1.0, 1.0],
                [1.0, 1.0, 1.0],
                [1.0, 1.0, -1.0],
            ],
        ),
        // −x
        (
            [-1.0, 0.0, 0.0],
            [
                [-1.0, -1.0, 1.0],
                [-1.0, -1.0, -1.0],
                [-1.0, 1.0, -1.0],
                [-1.0, 1.0, 1.0],
            ],
        ),
        // +y
        (
            [0.0, 1.0, 0.0],
            [
                [-1.0, 1.0, -1.0],
                [1.0, 1.0, -1.0],
                [1.0, 1.0, 1.0],
                [-1.0, 1.0, 1.0],
            ],
        ),
        // −y
        (
            [0.0, -1.0, 0.0],
            [
                [-1.0, -1.0, 1.0],
                [1.0, -1.0, 1.0],
                [1.0, -1.0, -1.0],
                [-1.0, -1.0, -1.0],
            ],
        ),
        // +z
        (
            [0.0, 0.0, 1.0],
            [
                [1.0, -1.0, 1.0],
                [-1.0, -1.0, 1.0],
                [-1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
            ],
        ),
        // −z
        (
            [0.0, 0.0, -1.0],
            [
                [-1.0, -1.0, -1.0],
                [1.0, -1.0, -1.0],
                [1.0, 1.0, -1.0],
                [-1.0, 1.0, -1.0],
            ],
        ),
    ];

    for (n, corners) in FACES {
        let base = buf.positions.len() as u32;
        let world_n = if n[1] == 0.0 { rotate(n[0], n[2]) } else { n };
        for c in corners {
            buf.positions.push(place(
                p.at[0] + c[0] * p.half[0],
                p.at[1] + c[1] * p.half[1],
                p.at[2] + c[2] * p.half[2],
            ));
            buf.normals.push(world_n);
            buf.colors.push(tint);
        }
        buf.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

// ─── Altitude LOD ────────────────────────────────────────────────────────────

/// How far the furniture has merged into the paving, 0 → 1 over
/// [`FURNITURE_FADE_START`]..[`FURNITURE_FADE_END`]. Read at spawn by
/// `map_geometry` so a cell streaming in above the band does not flash a street
/// full of benches for one frame.
/// The group's shared base colour at a given fade `factor`, in linear RGB.
///
/// Most street furniture stands on paving, so [`palette::ROAD_FILL`] is what it
/// merges into. Split out from the system so the convergence is a plain function
/// of the ramp and can be asserted on without a running app.
#[must_use]
pub fn paving_base(factor: f32) -> [f32; 3] {
    crate::scatter::mix(palette::FURNITURE, palette::ROAD_FILL, factor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_tiles::synth;

    #[test]
    fn a_surveyed_object_replaces_its_osm_twin_and_nothing_else() {
        let at = |x: f32, kind| FurnitureInstance {
            x,
            z: 0.0,
            yaw: 0.0,
            kind,
            luma: 1.0,
            paint: palette::FURNITURE,
            over_water: false,
            hull_only: false,
        };
        let surveyed = [at(0.0, FurnitureKind::Bin)];
        let mut pois = vec![
            at(1.5, FurnitureKind::Bin),   // the same bin, traced a little off
            at(6.0, FurnitureKind::Bin),   // the next bin along
            at(0.5, FurnitureKind::Bench), // a bench beside the bin
        ];
        drop_surveyed_twins(&mut pois, &surveyed);
        let left: Vec<(f32, FurnitureKind)> = pois.iter().map(|p| (p.x, p.kind)).collect();
        assert_eq!(
            left,
            [(6.0, FurnitureKind::Bin), (0.5, FurnitureKind::Bench)]
        );
    }

    /// The forms and the shared placeholder vocabulary are one list seen from two
    /// crates, and nothing but this test keeps them equal. A form missing a slug
    /// is a placeholder a curator can set and that overrides nothing; a slug with
    /// no form is a row the client silently drops.
    #[test]
    fn every_form_has_a_slug_the_wire_knows() {
        use cartopolis_core::placeholders::PLACEHOLDER_KINDS;
        let ours: std::collections::BTreeSet<&str> =
            ALL_FURNITURE_KINDS.iter().map(|k| k.slug()).collect();
        let theirs: std::collections::BTreeSet<&str> = PLACEHOLDER_KINDS.iter().copied().collect();
        assert_eq!(ours, theirs);
        assert_eq!(
            ours.len(),
            ALL_FURNITURE_KINDS.len(),
            "a slug is duplicated"
        );
    }

    /// The forms into thirty-two bits (a `u16` until the marina boats made
    /// seventeen). The test that fails the day a form too many is added, which
    /// is the only warning the mask can give.
    #[test]
    fn the_mask_holds_every_form() {
        assert!(
            ALL_FURNITURE_KINDS.len() <= 32,
            "FurnitureMask is a u32 and there are now {} forms",
            ALL_FURNITURE_KINDS.len()
        );
        let mut mask = FurnitureMask::NONE;
        assert!(mask.is_empty());
        for kind in ALL_FURNITURE_KINDS {
            assert!(!mask.contains(*kind));
            mask = mask.with(*kind);
            assert!(mask.contains(*kind));
        }
        assert!(!mask.is_empty());
    }

    #[test]
    fn a_slug_round_trips() {
        for kind in ALL_FURNITURE_KINDS {
            assert_eq!(FurnitureKind::from_slug(kind.slug()), Some(*kind));
        }
        assert_eq!(FurnitureKind::from_slug("lamppost"), None);
    }

    const TILE_M: f32 = 1000.0;

    // `extract_furniture` takes a `TileData` because on the worker it shares one
    // with every other consumer of the same tile body.
    use crate::testdata::groningen as fixture;

    #[test]
    fn the_fixture_furnishes_a_street() {
        let items = extract_furniture(&fixture(), TILE_M, 7).expect("extract");
        assert!(
            items.len() > 100,
            "a city tile should furnish more than a handful: {}",
            items.len()
        );
        let benches = items
            .iter()
            .filter(|i| i.kind == FurnitureKind::Bench)
            .count();
        assert!(benches > 50, "benches are the bulk of the layer: {benches}");
        // Every piece lands inside the cell it was read from — the tile buffer
        // duplicate would otherwise be drawn by two neighbouring cells at once.
        for i in &items {
            assert!(
                (0.0..TILE_M).contains(&i.x) && (0.0..TILE_M).contains(&i.z),
                "{:?} at ({}, {}) is outside the tile",
                i.kind,
                i.x,
                i.z
            );
        }
    }

    #[test]
    fn indoor_pois_are_left_undrawn() {
        // The layer is mostly shops, doctors and restaurants: points inside
        // buildings we already draw. Standing a box on each would put furniture
        // through the floor of every terrace on the map.
        let items = extract_furniture(&fixture(), TILE_M, 7).expect("extract");
        assert!(
            items.len() < 400,
            "the whole `pois` layer is 467 features; {} of them were furnished",
            items.len()
        );
    }

    #[test]
    fn extraction_is_stable_for_a_cell() {
        let a = extract_furniture(&fixture(), TILE_M, 11).expect("extract");
        let b = extract_furniture(&fixture(), TILE_M, 11).expect("extract");
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.kind, y.kind);
            assert!(
                (x.yaw - y.yaw).abs() < 1e-6,
                "a re-streamed cell re-rolled an angle"
            );
        }
    }

    #[test]
    fn benches_line_up_with_the_streets_they_stand_on() {
        // The claim this layer lives or dies on: a bench takes its angle from the
        // nearest path, not from a hash. Measured as "how many benches are within
        // 15° of *some* street direction" — a hashed angle would land ~17% of them
        // there by chance.
        let items = extract_furniture(&fixture(), TILE_M, 3).expect("extract");
        let ways = WayField::build(&fixture(), TILE_M);

        let benches: Vec<_> = items
            .iter()
            .filter(|i| i.kind == FurnitureKind::Bench)
            .collect();
        assert!(benches.len() > 50, "not enough benches to measure");

        // Independently of how the yaw was produced: find the nearest segment by
        // brute force, and check the bench's *long axis* ended up parallel to it.
        // A hashed angle would clear a 15° window about one in six times.
        let mut aligned = 0;
        let mut oriented = 0;
        for b in &benches {
            let Some((_, seg)) = ways
                .segs
                .iter()
                .map(|&(p, q)| (point_seg_dist2(b.x, b.z, p, q), (p, q)))
                .filter(|(d2, _)| *d2 <= ORIENT_RADIUS_M * ORIENT_RADIUS_M)
                .min_by(|a, c| a.0.partial_cmp(&c.0).unwrap())
            else {
                continue;
            };
            oriented += 1;
            // The bench's long axis is local +x, which the yaw maps to this.
            let (sn, cs) = b.yaw.sin_cos();
            let axis = (cs, -sn);
            let way = (seg.1[0] - seg.0[0], seg.1[1] - seg.0[1]);
            let len = (way.0 * way.0 + way.1 * way.1).sqrt();
            // |cos| of the angle between them: 1 is parallel, 0 perpendicular.
            let para = ((axis.0 * way.0 + axis.1 * way.1) / len).abs();
            if para > 0.966 {
                aligned += 1;
            }
        }
        assert!(oriented > 40, "only {oriented} benches had a way in range");
        assert!(
            aligned * 10 > oriented * 9,
            "only {aligned} of {oriented} benches lie along their nearest way"
        );
    }

    /// Where the object's front (+z) and long axis (+x) end up in world XZ.
    fn frame(yaw: f32) -> ((f32, f32), (f32, f32)) {
        let (sn, cs) = yaw.sin_cos();
        ((sn, cs), (cs, -sn))
    }

    #[test]
    fn a_bench_looks_at_the_path_from_either_side_of_it() {
        // One way running east along z = 0. A bench two metres south of it must
        // look north, and one two metres north of it must look south — in both
        // cases with its long axis running east–west, along the way.
        let mut ways = WayField::empty(200.0);
        ways.push([0.0, 0.0], [100.0, 0.0], None);

        let south = ways.facing(50.0, 2.0, ORIENT_RADIUS_M).expect("a path");
        let (front, axis) = frame(south);
        assert!(front.0.abs() < 1e-4 && front.1 < -0.99, "front {front:?}");
        assert!(axis.0.abs() > 0.99 && axis.1.abs() < 1e-4, "axis {axis:?}");

        let north = ways.facing(50.0, -2.0, ORIENT_RADIUS_M).expect("a path");
        let (front, axis) = frame(north);
        assert!(front.0.abs() < 1e-4 && front.1 > 0.99, "front {front:?}");
        assert!(axis.0.abs() > 0.99 && axis.1.abs() < 1e-4, "axis {axis:?}");
    }

    #[test]
    fn facing_is_a_quarter_turn_off_the_way_not_a_half_one() {
        // The bug this module was written around: lining a bench up *with* the
        // street direction points its long axis at the kerb. Whatever the way's
        // bearing, the front must come out perpendicular to it.
        for (dx, dz) in [(1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (-3.0, 1.0), (2.0, -5.0)] {
            // Wide enough that the grid clamp cannot reach the ±150 m
            // segments below; this test is about angles, not bounds.
            let mut ways = WayField::empty(600.0);
            ways.push([-dx * 50.0, -dz * 50.0], [dx * 50.0, dz * 50.0], None);
            let yaw = ways.facing(3.0, 3.0, ORIENT_RADIUS_M).expect("a path");
            let (front, _) = frame(yaw);
            let len = (dx * dx + dz * dz).sqrt();
            let along = (front.0 * dx + front.1 * dz) / len;
            assert!(
                along.abs() < 1e-4,
                "way ({dx}, {dz}): the front has a {along} component along it"
            );
        }
    }

    /// MVT declares an extent **per layer**, and the way field reads `streets`
    /// while its caller is walking `pois`. Handing it the caller's numbers put
    /// every street of such a tile at a fraction of its true distance, so a bench
    /// lined itself up with whatever happened to land near the wrong position —
    /// invisible on Shortbread, where every layer is 4096, and wrong on the first
    /// source that is not.
    #[test]
    fn the_way_field_is_scaled_by_the_streets_layers_own_extent() {
        let bytes = synth::tile(&[
            synth::line_layer("streets", 512, &[(0, 0), (512, 0)]),
            synth::line_layer("pois", 4096, &[(0, 0), (4096, 0)]),
        ]);
        let tile = TileData::parse(&bytes).expect("parse");
        let ways = WayField::build(&tile, 1000.0);

        assert_eq!(ways.segs.len(), 1, "one street segment expected");
        let (a, b) = ways.segs[0];
        assert!(a[0].abs() < 1e-3, "the street starts at {a:?}");
        // The street spans its whole tile, so in metres it must span the whole
        // 1000 m cell. Scaled by the `pois` extent instead it stops at 125 m.
        assert!(
            (b[0] - 1000.0).abs() < 1e-3,
            "the street ends at {b:?}, not at 1000 m"
        );
    }

    /// The field carries a half-carriageway per segment, and **only for a filter
    /// that was already reading the tags**.
    ///
    /// `All` is the furniture's path over every streamed cell and must keep
    /// costing no tag lookup per way, which is the rule stated on
    /// [`WayFilter::admits_feature`]. So it answers no width at all, and
    /// `crossings` — the one caller that wants one — is also the one already
    /// paying for the `kind`.
    #[test]
    fn only_a_filtered_field_carries_a_width() {
        let bytes = synth::tile(&[synth::tagged_line_layer(
            "streets",
            4096,
            ("kind", "secondary"),
            &[(0, 2048), (4095, 2048)],
        )]);
        let tile = TileData::parse(&bytes).expect("parse");

        let filtered = WayField::build_with(&tile, 400.0, WayFilter::ExcludingRail);
        assert_eq!(
            filtered.direction_and_width(200.0, 200.0, 30.0),
            Some((std::f32::consts::FRAC_PI_2, Some(5.5))),
            "a secondary road is 11 m wide, so half of it is 5.5"
        );

        let unfiltered = WayField::build_with(&tile, 400.0, WayFilter::All);
        assert_eq!(
            unfiltered
                .direction_and_width(200.0, 200.0, 30.0)
                .map(|(_, half)| half),
            Some(None),
            "the unfiltered field read a kind tag it must not pay for"
        );
    }

    /// One wild endpoint has to be enough to drop a segment.
    ///
    /// `mvt-reader` clips its coordinate cursor at `i32::MAX` and never to the
    /// layer's extent, so a few bytes of crafted varint put a vertex at ~2.1e9.
    /// The old test demanded that *both* endpoints be out of range before it
    /// rejected anything, so a segment from a plausible vertex to that one was
    /// kept — and the bbox loop in `push` then asked for ~10^14 buckets, each an
    /// allocation. The map is drawn from bytes a server we do not run handed us.
    #[test]
    fn a_way_running_off_to_two_billion_is_dropped() {
        let bytes = synth::tile(&[synth::line_layer(
            "streets",
            4096,
            &[(100, 100), (2_000_000_000, 5)],
        )]);
        let tile = TileData::parse(&bytes).expect("parse");
        let ways = WayField::build(&tile, TILE_M);
        assert!(ways.segs.is_empty(), "a segment running to 2e9 was kept");

        // …and the grid clamp bounds `push` even if a segment ever gets past the
        // window, which is the half that makes this a guarantee rather than a
        // filter that has to be complete.
        let mut field = WayField::empty(TILE_M);
        field.push([0.0, 0.0], [2.1e9, 2.1e9], None);
        assert!(
            field.buckets.len() < 20_000,
            "{} buckets from one segment",
            field.buckets.len()
        );
    }

    #[test]
    fn a_fountain_ignores_the_street_grid() {
        // Radially symmetric forms in open spaces take a hashed angle instead, so
        // a fountain does not square up to whichever side street passes nearest.
        assert!(!FurnitureKind::Fountain.takes_angle_from_path());
        assert!(!FurnitureKind::Artwork.takes_angle_from_path());
        assert!(FurnitureKind::Bench.takes_angle_from_path());
    }

    #[test]
    fn a_seat_is_the_top_of_a_slat_that_is_really_meshed() {
        // The avatar's sitting pose is built around `Seat::height`, and the only
        // thing that makes it land on the timber rather than through it is that
        // this number is the slat's own top. Nothing but this test connects the
        // two: `seat` restates the geometry, so a re-measured bench that did not
        // update it would seat the avatar in mid-air.
        for kind in [FurnitureKind::Bench, FurnitureKind::PicnicTable] {
            let s = seat(kind).expect("a form the prompt offers to sit at");
            for (z, _) in s.rows {
                let slat = parts(kind)
                    .iter()
                    .find(|p| (p.at[2] - z).abs() < 1e-3 && p.half[0] > s.half_span)
                    .unwrap_or_else(|| panic!("{kind:?} has no slat at z {z}"));
                assert!(
                    (slat.at[1] + slat.half[1] - s.height).abs() < 1e-3,
                    "{kind:?} seat at {} but the slat's top is {}",
                    s.height,
                    slat.at[1] + slat.half[1]
                );
            }
        }
        assert!(seat(FurnitureKind::Bin).is_none());
    }

    #[test]
    fn every_form_stands_on_the_ground_and_none_of_it_is_a_skyscraper() {
        for (_, _, kind) in FURNITURE_TABLE {
            let ps = parts(*kind);
            assert!(!ps.is_empty(), "{kind:?} has no geometry");
            let lowest = ps
                .iter()
                .map(|p| p.at[1] - p.half[1])
                .fold(f32::MAX, f32::min);
            let highest = ps
                .iter()
                .map(|p| p.at[1] + p.half[1])
                .fold(f32::MIN, f32::max);
            assert!(
                lowest >= -0.01,
                "{kind:?} reaches {lowest} m, below the ground plane"
            );
            assert!(
                lowest < 0.05,
                "{kind:?} floats: its lowest part is at {lowest} m"
            );
            assert!(
                (0.5..4.0).contains(&highest),
                "{kind:?} tops out at {highest} m, which is not street furniture"
            );
        }
    }

    #[test]
    fn a_merged_cell_is_one_mesh_with_matching_attribute_counts() {
        let items = extract_furniture(&fixture(), TILE_M, 5).expect("extract");
        let meshes = build_furniture_meshes(&items, None);
        assert_eq!(meshes.len(), 1, "furniture must merge into one draw");
        let m = &meshes[0];
        assert_eq!(m.group, SurfaceGroup::Furniture);
        assert_eq!(m.positions.len(), m.normals.len());
        assert_eq!(m.positions.len(), m.colors.len());
        assert!(m.uvs.is_empty(), "the group's material has no texture");
        assert_eq!(m.indices.len() % 3, 0);
        for &i in &m.indices {
            assert!((i as usize) < m.positions.len(), "index out of range");
        }
        // 24 verts per box, and every listed form is at least two boxes.
        assert!(m.positions.len() >= items.len() * 48);
    }

    #[test]
    fn empty_input_builds_nothing() {
        assert!(build_furniture_meshes(&[], None).is_empty());
    }

    #[test]
    fn a_rotated_box_keeps_its_size() {
        // The rotation is written out by hand; this is the check that it is a
        // rotation and not a shear.
        let one = [FurnitureInstance {
            x: 0.0,
            z: 0.0,
            yaw: 0.9,
            kind: FurnitureKind::Bin,
            luma: 1.0,
            paint: palette::FURNITURE,
            over_water: false,
            hull_only: false,
        }];
        let m = &build_furniture_meshes(&one, None)[0];
        let extent = |axis: usize| {
            let lo = m.positions.iter().map(|p| p[axis]).fold(f32::MAX, f32::min);
            let hi = m.positions.iter().map(|p| p[axis]).fold(f32::MIN, f32::max);
            hi - lo
        };
        // Every part of a bin is square in xz, so the widest one rotated by θ spans
        // its own width times (|cos θ| + |sin θ|). Derived from the form rather
        // than hardcoded, so re-proportioning a bin does not silently rot this into
        // a test of nothing — the claim under test is the *rotation*.
        let widest = form::BIN
            .iter()
            .map(|p| p.half[0].max(p.half[2]) * 2.0)
            .fold(0.0f32, f32::max);
        let span = widest * (0.9f32.cos().abs() + 0.9f32.sin().abs());
        assert!((extent(0) - span).abs() < 0.01, "x span {}", extent(0));
        assert!((extent(2) - span).abs() < 0.01, "z span {}", extent(2));
        // Height is unaffected by yaw: a shear would show up here and nowhere else.
        let tall = form::BIN
            .iter()
            .map(|p| p.at[1] + p.half[1])
            .fold(f32::MIN, f32::max);
        assert!((extent(1) - tall).abs() < 0.01, "y span {}", extent(1));
    }

    /// A measured height is what the wall is drawn at, and the nominal 2.0 m is
    /// only the answer when the register has nothing to say. Without this the
    /// median surveyed `muur` — 0.23 m of garden edging — is a two-metre slab, and
    /// every quay wall is the same two-metre slab as well.
    #[test]
    fn a_measured_boundary_is_drawn_at_its_measured_height() {
        let line = |height_m: Option<f32>| SurveyedLine {
            points: vec![[0.0, 0.0], [10.0, 0.0]],
            kind: LineKind::Wall,
            height_m,
        };
        let top = |l: SurveyedLine| {
            build_line_meshes(&[l], None)
                .first()
                .map(|m| m.positions.iter().fold(f32::MIN, |a, p| a.max(p[1])))
        };
        let nominal = top(line(None)).expect("the nominal form draws");
        assert!(
            (nominal - 0.9).abs() < 0.01,
            "an unmeasured wall keeps the form's height, got {nominal}"
        );
        assert!(
            nominal < 1.2,
            "the nominal wall must stay low enough to see over: at 2 m it walls off \
             every canal in the country, which is what it used to do"
        );
        let measured = top(line(Some(1.29))).expect("a measured wall draws");
        assert!(
            (measured - 1.29).abs() < 0.01,
            "a measured wall stands at its measurement, got {measured}"
        );
    }

    /// **A bank wall's top meets the ground behind it.** The register measures a
    /// wall from its foot, and beside water the foot is the lower side: a 2 m
    /// wall along a pond whose water lies 2 m down is flush with the walkway, not
    /// two metres above it. On level ground the foot is the ground itself and the
    /// wall is unchanged; a sink is never deeper than the wall is tall; and a long
    /// segment is cut into pieces so each can find its own foot.
    #[test]
    fn a_measured_wall_stands_on_its_lower_side() {
        let wall = SurveyedLine {
            points: vec![[0.0, 0.0], [50.0, 0.0]],
            kind: LineKind::Wall,
            height_m: Some(2.06),
        };
        let span = |m: &[SurfaceMesh]| {
            m.iter()
                .flat_map(|m| m.positions.iter())
                .fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                    (lo.min(p[1]), hi.max(p[1]))
                })
        };
        let level = build_line_meshes_footed(&[wall.clone()], None, &|_, _| 0.0);
        let (lo, hi) = span(&level);
        assert!(
            lo.abs() < 1e-4 && (hi - 2.06).abs() < 1e-4,
            "level ground: {lo}..{hi}"
        );
        let bank = build_line_meshes_footed(&[wall.clone()], None, &|_, _| 2.0);
        let (lo, hi) = span(&bank);
        assert!((lo + 2.0).abs() < 1e-4, "the foot is the water side: {lo}");
        assert!((hi - 0.06).abs() < 1e-4, "the top meets the walkway: {hi}");
        let deep = build_line_meshes_footed(&[wall.clone()], None, &|_, _| 9.0);
        let (_, hi) = span(&deep);
        assert!(hi.abs() < 1e-4, "never sunk past its own height: {hi}");
        let pieces: usize = bank.iter().map(|m| m.positions.len()).sum::<usize>() / 24;
        assert_eq!(pieces, 9, "50 m in pieces of at most {WALL_PIECE_M} m");
        let unmeasured = SurveyedLine {
            height_m: None,
            ..wall
        };
        let (lo, _) = span(&build_line_meshes_footed(
            &[unmeasured.clone()],
            None,
            &|_, _| 2.0,
        ));
        assert!(lo.abs() < 1e-4, "a nominal wall has no foot to find");
        // A quay wall is flush with the quay whatever the ground beside it says:
        // the survey line and the map's water rarely agree on where the edge is.
        let quay = SurveyedLine {
            kind: LineKind::Quay,
            height_m: Some(4.25),
            ..unmeasured
        };
        let (lo, hi) = span(&build_line_meshes_footed(&[quay], None, &|_, _| 0.0));
        assert!(hi.abs() < 1e-4 && (lo + 4.25).abs() < 1e-4, "{lo}..{hi}");
    }

    /// Below the floor there is no wall to draw. The 3D product drapes its objects
    /// on AHN, so a flat one comes back as a few centimetres of ground noise, and
    /// drawing that is a strip of z-fighting down the pavement rather than a low
    /// wall.
    #[test]
    fn a_boundary_measured_flat_is_not_drawn() {
        let flat = SurveyedLine {
            points: vec![[0.0, 0.0], [10.0, 0.0]],
            kind: LineKind::Wall,
            height_m: Some(0.08),
        };
        assert!(
            build_line_meshes(&[flat], None).is_empty(),
            "a boundary with no measurable height is skipped, not drawn flat"
        );
    }

    /// The jet and the basin are the same surveyed kind, and the water is what
    /// tells them apart. A basin at the waterline of a pond is what this replaced.
    #[test]
    fn a_fountain_in_water_is_drawn_as_a_jet() {
        let at = |over_water: bool| FurnitureInstance {
            x: 0.0,
            z: 0.0,
            yaw: 0.0,
            kind: FurnitureKind::Fountain,
            luma: 1.0,
            paint: palette::FURNITURE,
            over_water,
            hull_only: false,
        };
        assert_eq!(
            parts_for(&at(false)).len(),
            form::FOUNTAIN.len(),
            "on a square it is still the basin"
        );
        assert_eq!(
            parts_for(&at(true)).len(),
            form::FOUNTAIN_JET.len(),
            "in a pond it is a jet"
        );
        // The basin is a ring 3 m across; the jet is not. Its widest part must stay
        // under a metre or it reads as a basin that has lost its rim.
        let widest = form::FOUNTAIN_JET
            .iter()
            .fold(0.0f32, |a, p| a.max(p.half[0].max(p.half[2])));
        assert!(widest < 0.5, "the jet is narrow, got half-extent {widest}");
        // And the flag is only ever read for a fountain: a bench does not become a
        // different bench by standing near water.
        let bench = FurnitureInstance {
            kind: FurnitureKind::Bench,
            ..at(true)
        };
        assert_eq!(parts_for(&bench).len(), form::BENCH.len());
    }

    /// The water test against a real recorded tile, because the part that can be
    /// silently wrong is the frame: MVT declares an extent **per layer**, so a
    /// point arriving in the `pois` layer's coordinates has to be carried into the
    /// water layer's. Shortbread emits 4096 everywhere, which is exactly what makes
    /// the mistake invisible — the `WayField` trap in this module's own history is
    /// the same assumption, and it put benches at angles taken from streets that
    /// were nowhere near them.
    #[test]
    fn water_is_found_inside_a_real_canal() {
        use crate::testdata;
        let tile = testdata::groningen();

        // A point that is provably inside some water polygon in this tile: walk the
        // water layers, and for each exterior ring try its own vertices' mean. A
        // canal is convex enough that this lands inside for at least one of them.
        let mut inside: Option<(f32, f32)> = None;
        for meta in tile.layers() {
            if !WATER_LAYERS.contains(&meta.name.as_str()) {
                continue;
            }
            let extent = TileData::extent(meta);
            for feature in tile.features(meta) {
                let rings: Vec<&geo_types::Polygon<f32>> = match &feature.geometry {
                    geo_types::Geometry::Polygon(p) => vec![p],
                    geo_types::Geometry::MultiPolygon(mp) => mp.iter().collect(),
                    _ => continue,
                };
                for poly in rings {
                    let pts: Vec<(f32, f32)> =
                        poly.exterior().coords().map(|c| (c.x, c.y)).collect();
                    if pts.len() < 3 {
                        continue;
                    }
                    let n = pts.len() as f32;
                    let cx = pts.iter().map(|p| p.0).sum::<f32>() / n;
                    let cy = pts.iter().map(|p| p.1).sum::<f32>() / n;
                    let (u, v) = (cx / extent, cy / extent);
                    if (0.0..1.0).contains(&u)
                        && (0.0..1.0).contains(&v)
                        && water_under(&tile, u, v)
                    {
                        inside = Some((u, v));
                        break;
                    }
                }
                if inside.is_some() {
                    break;
                }
            }
            if inside.is_some() {
                break;
            }
        }
        let (u, v) = inside.expect("Groningen z14 has canals in it");
        assert!(water_under(&tile, u, v), "the point we just found is wet");

        // And the tile is not simply answering yes: most of a city is not water.
        let mut wet = 0;
        let mut total = 0;
        for i in 1..12 {
            for j in 1..12 {
                total += 1;
                wet += usize::from(water_under(&tile, i as f32 / 12.0, j as f32 / 12.0));
            }
        }
        assert!(
            wet < total / 2,
            "{wet} of {total} sample points wet — the frame is probably wrong"
        );
    }

    /// An operator's model replaces the basin and leaves the jet alone — and the
    /// jet must still be *meshed*, because the mask's whole meaning is "a model
    /// will stand here instead". Without the second half a fountain placeholder
    /// deletes every jet in the country.
    #[test]
    fn a_jet_refuses_a_placeholder_model_and_keeps_its_geometry() {
        let at = |over_water: bool| FurnitureInstance {
            x: 0.0,
            z: 0.0,
            yaw: 0.0,
            kind: FurnitureKind::Fountain,
            luma: 1.0,
            paint: palette::FURNITURE,
            over_water,
            hull_only: false,
        };
        assert!(at(false).takes_placeholder_model(), "a basin takes a model");
        assert!(!at(true).takes_placeholder_model(), "a jet does not");

        let skip = FurnitureMask::NONE.with(FurnitureKind::Fountain);
        assert!(
            build_furniture_meshes_except(&[at(false)], skip, None).is_empty(),
            "the basin is left to the model"
        );
        assert!(
            !build_furniture_meshes_except(&[at(true)], skip, None).is_empty(),
            "the jet is still drawn"
        );
    }

    #[test]
    fn normals_stay_unit_length_through_the_rotation() {
        let one = [FurnitureInstance {
            x: 3.0,
            z: -2.0,
            yaw: 2.1,
            kind: FurnitureKind::Shelter,
            luma: 1.0,
            paint: palette::FURNITURE,
            over_water: false,
            hull_only: false,
        }];
        let m = &build_furniture_meshes(&one, None)[0];
        for n in &m.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "normal {n:?} has length {len}");
        }
    }

    #[test]
    fn the_fade_walks_every_tone_toward_the_paving() {
        // What the ramp has to buy: by the top of the band each object is much
        // closer in tone to the ground it stands on than it was at the bottom, so
        // hiding the geometry is a non-event rather than a pop. It does not reach
        // zero — see `update_furniture_lod` for why that residue is affordable.
        let ground = srgb_to_linear_rgb(palette::ROAD_FILL);
        let gap = |tone: Tone, factor: f32| {
            let base = paving_base(factor);
            let m = tone.multiplier();
            (0..3)
                .map(|i| (m[i] * base[i] - ground[i]).abs())
                .fold(0.0f32, f32::max)
        };
        for tone in [Tone::Wood, Tone::Metal, Tone::Roof, Tone::Stone] {
            let (near, far) = (gap(tone, 0.0), gap(tone, 1.0));
            assert!(
                far < near,
                "{tone:?} does not close on the paving: {near} → {far}"
            );
        }
        // The base itself lands exactly on the paving at the top of the band, and
        // starts exactly on the group's own colour at the bottom.
        assert_eq!(paving_base(1.0), ground);
        assert_eq!(paving_base(0.0), srgb_to_linear_rgb(palette::FURNITURE));
    }

    #[test]
    fn the_tones_are_muted_against_the_paper() {
        // The palette rule this module inherits: nothing here may be more saturated
        // than the buildings already on the map, or a bench becomes the loudest
        // thing on screen.
        let saturation = |rgb: u32| {
            let c = [
                ((rgb >> 16) & 0xFF) as f32,
                ((rgb >> 8) & 0xFF) as f32,
                (rgb & 0xFF) as f32,
            ];
            let hi = c[0].max(c[1]).max(c[2]);
            let lo = c[0].min(c[1]).min(c[2]);
            if hi <= 0.0 {
                0.0
            } else {
                (hi - lo) / hi
            }
        };
        // Water is exempt: it is the canal colour, which the map already carries.
        for tone in [Tone::Wood, Tone::Metal, Tone::Roof, Tone::Stone] {
            let s = saturation(tone.rgb());
            assert!(
                s <= saturation(palette::TRUNK) + 0.02,
                "{tone:?} at saturation {s} is louder than a tree trunk"
            );
        }
    }
}
