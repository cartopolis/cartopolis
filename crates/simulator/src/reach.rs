//! What is within reach of a person, and the table a tick leaves behind for the
//! connections to assemble their snapshots from (`docs/notes/interest-by-reach.md`).
//!
//! Every avatar and every thing has a place on Earth. A connection is sent the
//! avatars and things within a radius of its own avatar — [`reach_m`] — and
//! nothing else: not the whole server, as the broadcast used to carry, and not
//! the contents of some cell the avatar happens to stand in. There are no cells
//! here, on purpose (`docs/mockups/federation/README.md`, *Rejected*): a reach
//! is a circle about a person, reaches overlap freely, and no reach owns
//! anything.
//!
//! The index is a list sorted by latitude. A query takes the band of latitudes
//! a radius spans — a binary search either side — and measures the ground
//! distance to each entry in it. At the populations one server holds that is
//! faster than any tree would be to build twenty times a second, and it has no
//! metric to get wrong at a pole or across the antimeridian: the band is in
//! degrees of latitude, which are the same length everywhere, and the distance
//! is `cartopolis_core::ground_distance_m`'s. Swap the representation here if a
//! measurement ever says so; nothing outside this module knows what it is.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use cartopolis_core::GeoAnchor;

/// A place on Earth with a height above the sim's flat ground: where an avatar
/// stands, or a thing sits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Place {
    pub geo: GeoAnchor,
    pub alt_m: f32,
}

impl Place {
    #[must_use]
    pub fn new(latitude: f64, longitude: f64, alt_m: f32) -> Self {
        Self {
            geo: GeoAnchor {
                latitude,
                longitude,
            },
            alt_m,
        }
    }

    /// Whether this is a place at all: finite and on the globe. Everything that
    /// takes a place from a client checks this first, because a NaN latitude
    /// is a distance of NaN to everyone, which no range test accepts and no
    /// range test rejects either.
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.geo.latitude.is_finite()
            && self.geo.longitude.is_finite()
            && self.alt_m.is_finite()
            && self.geo.latitude.abs() <= 90.0
            && self.geo.longitude.abs() <= 180.0
    }

    /// Metres between two places: the ground distance and the height
    /// difference as one straight line, so a listener sixty metres up is as far
    /// as one sixty metres away.
    #[must_use]
    pub fn distance_m(self, other: Place) -> f32 {
        let ground = ground_m(self.geo, other.geo);
        let dy = self.alt_m - other.alt_m;
        (ground * ground + dy * dy).sqrt()
    }
}

/// Ground distance between two anchors, as `f32`; infinite when either is not a
/// number, so that no range test accepts it.
#[must_use]
pub fn ground_m(a: GeoAnchor, b: GeoAnchor) -> f32 {
    if !a.latitude.is_finite()
        || !a.longitude.is_finite()
        || !b.latitude.is_finite()
        || !b.longitude.is_finite()
    {
        return f32::INFINITY;
    }
    cartopolis_core::ground_distance_m(a.latitude, a.longitude, b.latitude, b.longitude) as f32
}

/// How far past the shout radius a connection is sent the world, so that
/// somebody walking towards you is drawn before you can hear them, and a thing
/// is in place before you reach it.
pub const REACH_MARGIN_M: f32 = 200.0;

/// What a person standing on the ground receives: everything within the shout
/// radius and the margin beyond it.
pub const INTEREST_RADIUS_M: f32 = crate::chat::SHOUT_RADIUS_M + REACH_MARGIN_M;

/// The most a reach may widen to, whatever the altitude.
pub const MAX_REACH_M: f32 = 5_000.0;

/// How far a person at a height can see: the ground reach, widened by twice the
/// altitude — a view from a hundred metres up takes in a few hundred metres of
/// ground — and capped at [`MAX_REACH_M`]. The map streams by altitude the same
/// way, so what is drawn under a flying avatar is matched by who is drawn on it.
#[must_use]
pub fn reach_m(alt_m: f32) -> f32 {
    (INTEREST_RADIUS_M + 2.0 * alt_m.max(0.0)).min(MAX_REACH_M)
}

/// Places keyed by `K`, answering "which are within `radius` of here".
#[derive(Debug, Clone)]
pub struct ReachIndex<K> {
    /// Sorted by latitude.
    entries: Vec<(f64, K, Place)>,
}

impl<K: Copy> Default for ReachIndex<K> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<K: Copy> ReachIndex<K> {
    /// Build from every place in `items`; an invalid one is left out, so a
    /// query never has to ask.
    pub fn build(items: impl IntoIterator<Item = (K, Place)>) -> Self {
        let mut entries: Vec<(f64, K, Place)> = items
            .into_iter()
            .filter(|(_, p)| p.is_valid())
            .map(|(k, p)| (p.geo.latitude, k, p))
            .collect();
        entries.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self { entries }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every entry within `radius_m` of `centre`, with its distance, nearest
    /// first. `centre` itself is included if it is an entry.
    pub fn within(&self, centre: Place, radius_m: f32) -> Vec<(K, f32)> {
        if !centre.is_valid() || !radius_m.is_finite() || radius_m < 0.0 {
            return Vec::new();
        }
        // The band of latitudes the radius can span. A degree of latitude is
        // 110.57 km at the equator and 111.69 km at a pole; the shorter one makes
        // the band generous rather than tight, and the exact test below pays
        // for it.
        let band = f64::from(radius_m) / 110_000.0;
        let lo = centre.geo.latitude - band;
        let hi = centre.geo.latitude + band;
        let from = self.entries.partition_point(|e| e.0 < lo);
        let to = self.entries.partition_point(|e| e.0 <= hi);
        let mut out: Vec<(K, f32)> = self.entries[from..to]
            .iter()
            .filter_map(|(_, k, p)| {
                let d = centre.distance_m(*p);
                (d <= radius_m).then_some((*k, d))
            })
            .collect();
        out.sort_by(|a, b| a.1.total_cmp(&b.1));
        out
    }
}

/// One tick's world, as the connections read it: every placed avatar's state
/// encoded once, and the places of avatars and things in two indexes. Built by
/// the tick loop after it releases the world lock, shared with every connection
/// as one `Arc`, and read by each to assemble the snapshot for its own reach
/// ([`crate::net::reach`]).
#[derive(Debug)]
pub struct TickTable {
    pub tick: u64,
    /// Each placed avatar's `AvatarStateDto`, as the bytes it occupies in a
    /// `WorldSnapshot` (`cartopolis_core::encode_avatar_state`).
    states: Vec<Bytes>,
    /// Avatar id -> its place, and its row in `states`.
    avatars: HashMap<u64, (Place, usize)>,
    /// Avatars by place, keyed by their row in `states`.
    avatar_index: ReachIndex<usize>,
    /// Things by place.
    things: ReachIndex<i64>,
}

impl TickTable {
    pub fn new(
        tick: u64,
        avatars: Vec<(u64, Place, Bytes)>,
        things: impl IntoIterator<Item = (i64, Place)>,
    ) -> Self {
        let mut states = Vec::with_capacity(avatars.len());
        let mut by_id = HashMap::with_capacity(avatars.len());
        let mut placed = Vec::with_capacity(avatars.len());
        for (id, place, bytes) in avatars {
            let row = states.len();
            states.push(bytes);
            by_id.insert(id, (place, row));
            placed.push((row, place));
        }
        Self {
            tick,
            states,
            avatars: by_id,
            avatar_index: ReachIndex::build(placed),
            things: ReachIndex::build(things),
        }
    }

    /// Where an avatar stands this tick, if it is placed.
    #[must_use]
    pub fn place_of(&self, avatar_id: u64) -> Option<Place> {
        self.avatars.get(&avatar_id).map(|(p, _)| *p)
    }

    /// The encoded states of the avatars within `radius_m` of `centre`.
    pub fn avatars_within(&self, centre: Place, radius_m: f32) -> Vec<&[u8]> {
        self.avatar_index
            .within(centre, radius_m)
            .into_iter()
            .map(|(row, _)| self.states[row].as_ref())
            .collect()
    }

    /// The ids of the things within `radius_m` of `centre`.
    pub fn things_within(&self, centre: Place, radius_m: f32) -> impl Iterator<Item = i64> + '_ {
        self.things
            .within(centre, radius_m)
            .into_iter()
            .map(|(id, _)| id)
    }

    /// How many avatars are placed this tick.
    #[must_use]
    pub fn avatar_count(&self) -> usize {
        self.states.len()
    }
}

/// What the tick loop and the prim service hand every connection, on one
/// channel so that order between them is kept.
///
/// It used to be bare encoded frames, every one for everybody. Two kinds are not
/// for everybody any more: a thing's change is for the connections whose reach
/// holds it, and the tick is for each connection to cut its own snapshot from.
#[derive(Debug, Clone)]
pub enum Outbound {
    /// An encoded frame every connection is sent as it is: the capacity, a
    /// prim's motion, tone, sound or console line, a look.
    Frame(Bytes),
    /// A thing was deleted: the encoded `PrimRemoved`, for everybody, and the
    /// id so a connection that had it within reach forgets it.
    Removed { id: i64, frame: Bytes },
    /// A thing was made or changed: forwarded to the connections whose reach
    /// holds `place`, as the encoded `PrimUpsert` it already is — and to the
    /// account that asked for the change (`by`), wherever it stands, because a
    /// create has no other answer and an edit's answer must reach the editor.
    Thing {
        id: i64,
        place: Place,
        frame: Bytes,
        by: Option<String>,
    },
    /// This tick's world, for each connection to take its reach from.
    Tick(Arc<TickTable>),
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAT: f64 = 53.2194;
    const LNG: f64 = 6.5665;

    fn metres_east(m: f64) -> Place {
        let (_, per_lng) = cartopolis_core::degree_scale_m(LAT);
        Place::new(LAT, LNG + m / per_lng, 0.0)
    }

    fn metres_north(m: f64) -> Place {
        let (per_lat, _) = cartopolis_core::degree_scale_m(LAT);
        Place::new(LAT + m / per_lat, LNG, 0.0)
    }

    #[test]
    fn within_is_a_circle_about_the_centre_nearest_first() {
        let index = ReachIndex::build([
            (1u32, metres_east(10.0)),
            (2, metres_north(-490.0)),
            (3, metres_east(510.0)),
            (4, metres_north(2_000.0)),
            (5, Place::new(LAT, LNG, 0.0)),
        ]);
        let got = index.within(Place::new(LAT, LNG, 0.0), INTEREST_RADIUS_M);
        let ids: Vec<u32> = got.iter().map(|(k, _)| *k).collect();
        assert_eq!(ids, vec![5, 1, 2], "{got:?}");
        assert!((got[1].1 - 10.0).abs() < 0.1, "{got:?}");
        assert!((got[2].1 - 490.0).abs() < 1.0, "{got:?}");
    }

    #[test]
    fn height_counts_as_distance() {
        let index = ReachIndex::build([(1, Place::new(LAT, LNG, 60.0)), (2, metres_east(42.0))]);
        let ids: Vec<i32> = index
            .within(Place::new(LAT, LNG, 0.0), crate::chat::SAY_RADIUS_M)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            ids,
            vec![2],
            "60 m up is out of say range; 42 m across is in"
        );
    }

    #[test]
    fn an_invalid_place_is_neither_indexed_nor_a_centre() {
        let index = ReachIndex::build([
            (1, Place::new(f64::NAN, LNG, 0.0)),
            (2, Place::new(LAT, 200.0, 0.0)),
            (3, Place::new(LAT, LNG, f32::INFINITY)),
            (4, Place::new(LAT, LNG, 0.0)),
        ]);
        assert_eq!(index.len(), 1);
        assert!(index
            .within(Place::new(f64::NAN, LNG, 0.0), 1_000.0)
            .is_empty());
        assert_eq!(index.within(Place::new(LAT, LNG, 0.0), 1.0).len(), 1);
    }

    #[test]
    fn the_antimeridian_and_the_poles_are_not_special() {
        let index = ReachIndex::build([
            (1, Place::new(0.0, 179.999, 0.0)),
            (2, Place::new(89.9995, 0.0, 0.0)),
            (3, Place::new(89.9995, 180.0, 0.0)),
        ]);
        // 0.002° of longitude at the equator is about 220 m, across the seam.
        let across = index.within(Place::new(0.0, -179.999, 0.0), 300.0);
        assert_eq!(across.len(), 1, "{across:?}");
        assert_eq!(across[0].0, 1);
        // Two places 0.0005° off the pole on opposite meridians are about 110 m
        // apart, whatever their longitudes say.
        let polar = index.within(Place::new(89.9995, 90.0, 0.0), 200.0);
        let ids: Vec<i32> = polar.iter().map(|(k, _)| *k).collect();
        assert_eq!(ids.len(), 2, "{polar:?}");
        assert!(ids.contains(&2) && ids.contains(&3));
    }

    #[test]
    fn reach_widens_with_altitude_to_a_ceiling() {
        assert_eq!(reach_m(0.0), INTEREST_RADIUS_M);
        assert_eq!(
            reach_m(-5.0),
            INTEREST_RADIUS_M,
            "below ground widens nothing"
        );
        assert!(reach_m(100.0) > INTEREST_RADIUS_M);
        assert_eq!(reach_m(1e6), MAX_REACH_M);
    }

    #[test]
    fn a_tick_table_answers_by_reach() {
        let here = Place::new(LAT, LNG, 0.0);
        let table = TickTable::new(
            7,
            vec![
                (10, here, Bytes::from_static(b"me")),
                (11, metres_east(100.0), Bytes::from_static(b"near")),
                (12, metres_east(900.0), Bytes::from_static(b"far")),
            ],
            [(1i64, metres_north(50.0)), (2, metres_north(5_000.0))],
        );
        assert_eq!(table.place_of(10), Some(here));
        assert_eq!(table.place_of(99), None);
        let states = table.avatars_within(here, reach_m(0.0));
        assert_eq!(states, vec![b"me".as_slice(), b"near".as_slice()]);
        let things: Vec<i64> = table.things_within(here, reach_m(0.0)).collect();
        assert_eq!(things, vec![1]);
    }
}
