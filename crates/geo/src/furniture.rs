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
//! * **Mass mesh churn loses the device** (CLAUDE.md). Furniture is merged into
//!   *one* mesh per cell, rides `map_geometry`'s existing worker, build cap and
//!   despawn cap, and is hard-capped per cell by [`MAX_FURNITURE_PER_CELL`].
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
}

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

        out.push(FurnitureInstance {
            x,
            z,
            yaw,
            kind,
            luma: 1.0 + (rand01(h, 1) - 0.5) * 0.10,
            // No form here has a `Paint` part; this is the no-tint value.
            paint: palette::FURNITURE,
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
    pub const RECYCLING: [Part; 2] = [
        part([0.0, 0.70, 0.0], [0.75, 0.70, 0.65], Metal),
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
    }
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
    points: &[(f32, f32, FurnitureKind)],
) -> Vec<FurnitureInstance> {
    if points.is_empty() {
        return Vec::new();
    }
    let ways = WayField::build(tile, tile_m);
    let mut out = Vec::with_capacity(points.len());
    for (i, &(x, z, kind)) in points.iter().enumerate() {
        let h = hash3(seed, i as i32, kind as i32);
        let yaw = kind
            .takes_angle_from_path()
            .then(|| ways.facing(x, z, ORIENT_RADIUS_M))
            .flatten()
            .unwrap_or_else(|| rand01(h, 0) * std::f32::consts::TAU);
        out.push(FurnitureInstance {
            x,
            z,
            yaw,
            kind,
            luma: 1.0 + (rand01(h, 1) - 0.5) * 0.10,
            paint: palette::FURNITURE,
        });
    }
    out
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
#[must_use]
pub fn build_furniture_meshes(
    items: &[FurnitureInstance],
    max_bytes: Option<usize>,
) -> Vec<SurfaceMesh> {
    if items.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = MeshBuf::default();
    for item in items {
        // Flush *before* adding, and only with something to flush — a piece must
        // hold at least one object or a cap under one form's cost would emit
        // empty meshes forever.
        if let Some(max) = max_bytes {
            let step = parts(item.kind).len() * BOX_BYTES;
            if buf.bytes() + step > max && !buf.positions.is_empty() {
                out.push(std::mem::take(&mut buf).finish(SurfaceGroup::Furniture));
            }
        }
        let (sn, cs) = item.yaw.sin_cos();
        for p in parts(item.kind) {
            let m = match p.tone {
                Tone::Paint => paint_multiplier(item.paint),
                tone => tone.multiplier(),
            };
            let tint = [m[0] * item.luma, m[1] * item.luma, m[2] * item.luma, 1.0];
            push_box(&mut buf, item.x, item.z, (sn, cs), p, tint);
        }
    }
    if !buf.positions.is_empty() {
        out.push(buf.finish(SurfaceGroup::Furniture));
    }
    out
}

/// A surveyed boundary: a fence, a wall or a hedge, as the line it was surveyed
/// as.
///
/// Points are tile-local metres, the frame everything else here works in.
#[derive(Debug, Clone)]
pub struct SurveyedLine {
    pub points: Vec<[f32; 2]>,
    pub kind: LineKind,
}

/// The three linear forms. Mirrors `LINE_KINDS` in cartopy's `bgt.ts`.
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
            Self::Wall => (2.0, 0.09, Tone::Stone),
            Self::Hedge => (1.1, 0.35, Tone::Foliage),
        }
    }
}

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
    if lines.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = MeshBuf::default();
    for line in lines {
        let (height, half_thick, tone) = line.kind.spec();
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
            if let Some(max) = max_bytes {
                if buf.bytes() + BOX_BYTES > max && !buf.positions.is_empty() {
                    out.push(std::mem::take(&mut buf).finish(SurfaceGroup::Furniture));
                }
            }
            // `push_box` maps the box's local +x onto world `(cos, -sin)`, so the
            // rotation that lays a box along `(dx, dz)` is this one. Same
            // convention as [`WayField::facing`], which maps local +z instead —
            // the two are a quarter turn apart and both are stated where they are
            // used, because guessing wrong lays every fence across its own line.
            let (ux, uz) = (dx / len, dz / len);
            let (sn, cs) = (-uz, ux);
            let part = Part {
                at: [0.0, height * 0.5, 0.0],
                half: [len * 0.5, height * 0.5, half_thick],
                tone,
            };
            push_box(
                &mut buf,
                (a[0] + b[0]) * 0.5,
                (a[1] + b[1]) * 0.5,
                (sn, cs),
                &part,
                tint,
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
    let place = |lx: f32, ly: f32, lz: f32| [ox + lx * cs + lz * sn, ly, oz - lx * sn + lz * cs];
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

    const GRONINGEN_Z14: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../geo/tests/fixtures/groningen_z14.mvt"
    ));
    const TILE_M: f32 = 1000.0;

    /// The fixture, parsed. `extract_furniture` takes a `TileData` because on the
    /// worker it shares one with every other consumer of the same tile body.
    fn fixture() -> TileData {
        TileData::parse(GRONINGEN_Z14).expect("parse fixture")
    }

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

    #[test]
    fn normals_stay_unit_length_through_the_rotation() {
        let one = [FurnitureInstance {
            x: 3.0,
            z: -2.0,
            yaw: 2.1,
            kind: FurnitureKind::Shelter,
            luma: 1.0,
            paint: palette::FURNITURE,
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
