//! Boards: what a scenario's places are.
//!
//! A primitive is placed on a board at the scale it is native to (v4-whole):
//! **official areas** (provinces, municipalities, safety regions, CBS buurten —
//! the areas an authority speaks for), **cells** (H3, for what spreads over the
//! ground), or a **network** (nodes and links: rail, the power grid, planets).
//! A place on a board is named by a string the board understands: an area's
//! code, an H3 index in hex, a node's id.
//!
//! Only official areas carry geometry so far ([`AreaSet`]); cells and networks
//! can already be named, so a scenario written for them parses and its explicit
//! states play, but nothing derives a state on them yet.

use crate::ladder::Rung;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A board, as a scenario names it: `"areas:ne-admin1"`, `"h3:5"`,
/// `"network:nl-rail"`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Board {
    /// A set of official areas, by the set's name ([`AreaSet::name`]).
    Areas(String),
    /// H3 cells of one resolution.
    Cells { res: u8 },
    /// A named network of nodes and links.
    Network(String),
}

impl FromStr for Board {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let (kind, name) = s
            .split_once(':')
            .ok_or_else(|| format!("board {s:?}: expected kind:name"))?;
        match kind {
            "areas" if !name.is_empty() => Ok(Board::Areas(name.to_owned())),
            "h3" => match name.parse::<u8>() {
                Ok(res) if res <= 15 => Ok(Board::Cells { res }),
                _ => Err(format!("board {s:?}: H3 resolutions run 0 to 15")),
            },
            "network" if !name.is_empty() => Ok(Board::Network(name.to_owned())),
            _ => Err(format!("board {s:?}: kinds are areas, h3 and network")),
        }
    }
}

impl fmt::Display for Board {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Board::Areas(n) => write!(f, "areas:{n}"),
            Board::Cells { res } => write!(f, "h3:{res}"),
            Board::Network(n) => write!(f, "network:{n}"),
        }
    }
}

impl<'de> Deserialize<'de> for Board {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// A longitude and latitude, degrees.
pub type LonLat = [f64; 2];

/// Coordinates in a stored area file are whole units of this many degrees
/// (about a metre), written as differences along each ring.
const QUANTUM: f64 = 1e-5;

/// What the first bytes of an area file say, so a wrong file fails loudly.
const MAGIC: &[u8; 8] = b"CARTAREA";

/// Bump when the stored layout changes.
pub const AREA_FORMAT: u32 = 2;

/// One area as stored: rings of quantised, delta-coded coordinates.
#[derive(Serialize, Deserialize)]
struct StoredArea {
    id: String,
    name: String,
    country: String,
    /// Polygons; each polygon's first ring is its outline, the rest holes.
    polygons: Vec<Vec<Vec<i32>>>,
    parent: Option<String>,
    values: Vec<f32>,
}

#[derive(Serialize, Deserialize)]
struct StoredSet {
    format: u32,
    name: String,
    attribution: String,
    rung: Rung,
    fields: Vec<Field>,
    areas: Vec<StoredArea>,
}

/// How a figure is carried up to the rung above.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Roll {
    /// Counts add up: inhabitants, homes, check-ins.
    Sum,
    /// Shares and averages are weighted by another field of the same set: a
    /// share of people by the inhabitants, a mean house value by the homes.
    Mean { weight: String },
    /// Not carried up.
    Keep,
}

/// One figure an area set carries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// What layers call it (`share_65_plus`).
    pub key: String,
    /// What a person reads ("Aged 65 and over").
    pub label: String,
    /// `"%"`, `"people"`, `"k€"`.
    pub unit: String,
    pub roll: Roll,
    /// The year it describes, when it describes one.
    pub year: Option<i32>,
}

/// One official area.
#[derive(Clone, Debug)]
pub struct Area {
    /// The publisher's code (`POL-1851`, `GM0200`).
    pub id: String,
    pub name: String,
    /// ISO 3166 alpha-3 of the country it lies in.
    pub country: String,
    /// Polygons, each its outline then its holes, closed implicitly.
    pub polygons: Vec<Vec<Vec<LonLat>>>,
    /// `[west, south, east, north]`.
    pub bbox: [f64; 4],
    /// The code of the area it lies in on the rung above, in the set that
    /// rolls it up (a buurt's wijk, a gemeente's provincie).
    pub parent: Option<String>,
    /// One value per field of its set; NaN where nothing is published (a
    /// figure kept back because it would point at a few people, or unknown).
    pub values: Vec<f32>,
}

impl Area {
    #[must_use]
    pub fn new(id: String, name: String, country: String, polygons: Vec<Vec<Vec<LonLat>>>) -> Self {
        let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for p in polygons.iter().flat_map(|p| p.first()).flatten() {
            bbox[0] = bbox[0].min(p[0]);
            bbox[1] = bbox[1].min(p[1]);
            bbox[2] = bbox[2].max(p[0]);
            bbox[3] = bbox[3].max(p[1]);
        }
        Area {
            id,
            name,
            country,
            polygons,
            bbox,
            parent: None,
            values: Vec::new(),
        }
    }

    /// A point inside the area, for placing it in a coarser one: the middle of
    /// the widest stretch of its largest polygon along the line of latitude
    /// through that polygon's middle. A centroid can fall outside a crescent.
    #[must_use]
    pub fn inner_point(&self) -> Option<LonLat> {
        let outer = self
            .polygons
            .iter()
            .filter_map(|p| p.first())
            .max_by(|a, b| ring_area(a).total_cmp(&ring_area(b)))?;
        let (s, n) = outer
            .iter()
            .fold((f64::MAX, f64::MIN), |(s, n), p| (s.min(p[1]), n.max(p[1])));
        let y = (s + n) / 2.0;
        let mut xs: Vec<f64> = Vec::new();
        for i in 0..outer.len() {
            let (a, b) = (outer[i], outer[(i + 1) % outer.len()]);
            if (a[1] > y) != (b[1] > y) {
                xs.push(a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
            }
        }
        xs.sort_by(f64::total_cmp);
        xs.chunks_exact(2)
            .max_by(|a, b| (a[1] - a[0]).total_cmp(&(b[1] - b[0])))
            .map(|c| [(c[0] + c[1]) / 2.0, y])
    }

    /// Whether a point lies in the area (even–odd over each polygon's rings).
    #[must_use]
    pub fn contains(&self, p: LonLat) -> bool {
        let [w, s, e, n] = self.bbox;
        if p[0] < w || p[0] > e || p[1] < s || p[1] > n {
            return false;
        }
        self.polygons
            .iter()
            .any(|rings| rings.iter().filter(|ring| ring_contains(ring, p)).count() % 2 == 1)
    }

    /// The area as triangles, for filling: vertices and index triples.
    #[must_use]
    pub fn triangles(&self) -> (Vec<LonLat>, Vec<u32>) {
        let mut verts: Vec<LonLat> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        for rings in &self.polygons {
            let mut flat: Vec<f64> = Vec::new();
            let mut holes: Vec<usize> = Vec::new();
            for (i, ring) in rings.iter().enumerate() {
                if i > 0 {
                    holes.push(flat.len() / 2);
                }
                flat.extend(ring.iter().flat_map(|p| [p[0], p[1]]));
            }
            let base = verts.len() as u32;
            let Ok(tris) = earcutr::earcut(&flat, &holes, 2) else {
                continue;
            };
            verts.extend(flat.chunks_exact(2).map(|c| [c[0], c[1]]));
            idx.extend(tris.into_iter().map(|t| base + t as u32));
        }
        (verts, idx)
    }
}

fn ring_area(ring: &[LonLat]) -> f64 {
    let mut a = 0.0;
    for i in 0..ring.len() {
        let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
        a += p[0] * q[1] - q[0] * p[1];
    }
    (a / 2.0).abs()
}

fn ring_contains(ring: &[LonLat], p: LonLat) -> bool {
    let mut inside = false;
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if p[0] < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// A set of official areas: one board's geometry.
#[derive(Clone, Debug)]
pub struct AreaSet {
    /// What scenarios call it (`ne-admin1`); a scenario's `areas:<name>`.
    pub name: String,
    /// The publisher's credit, shown wherever the areas are.
    pub attribution: String,
    /// The rung its places stand on.
    pub rung: Rung,
    /// The figures each area carries, in the order of [`Area::values`].
    pub fields: Vec<Field>,
    pub areas: Vec<Area>,
    /// Area indices by 1° cell of the bbox, for [`AreaSet::area_at`].
    grid: std::collections::HashMap<(i32, i32), Vec<u32>>,
}

/// Why an area file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum AreaFileError {
    #[error("not an area file")]
    NotAnAreaFile,
    #[error("area file format {0}, this build reads {AREA_FORMAT}")]
    Format(u32),
    #[error("area file: {0}")]
    Decode(#[from] postcard::Error),
}

impl AreaSet {
    #[must_use]
    pub fn new(name: String, attribution: String, areas: Vec<Area>) -> Self {
        let mut grid: std::collections::HashMap<(i32, i32), Vec<u32>> = Default::default();
        for (i, a) in areas.iter().enumerate() {
            let [w, s, e, n] = a.bbox;
            for x in w.floor() as i32..=e.floor() as i32 {
                for y in s.floor() as i32..=n.floor() as i32 {
                    grid.entry((x, y)).or_default().push(i as u32);
                }
            }
        }
        AreaSet {
            name,
            attribution,
            rung: Rung::Province,
            fields: Vec::new(),
            areas,
            grid,
        }
    }

    #[must_use]
    pub fn with_rung(mut self, rung: Rung) -> Self {
        self.rung = rung;
        self
    }

    /// Give the set its fields; every area's values must follow their order.
    #[must_use]
    pub fn with_fields(mut self, fields: Vec<Field>) -> Self {
        self.fields = fields;
        self
    }

    /// A field's index by its key: the first, for a figure of one year.
    #[must_use]
    pub fn field(&self, key: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.key == key)
    }

    /// The field of `key` that describes `year`. A **series** is one field per
    /// year under one key (a forecast's population in 2023 … 2050): its latest
    /// year at or before `year`, else its earliest. A figure that names no year
    /// answers every year, and with no year asked the latest is given.
    #[must_use]
    pub fn field_at(&self, key: &str, year: Option<i32>) -> Option<usize> {
        let of_key = || {
            self.fields
                .iter()
                .enumerate()
                .filter(move |(_, f)| f.key == key)
        };
        let latest = |cap: Option<i32>| {
            of_key()
                .filter(|(_, f)| match (f.year, cap) {
                    (Some(y), Some(c)) => y <= c,
                    _ => true,
                })
                .max_by_key(|(_, f)| f.year)
                .map(|(i, _)| i)
        };
        latest(year).or_else(|| of_key().min_by_key(|(_, f)| f.year).map(|(i, _)| i))
    }

    /// The years a key's series is published for, rising; empty for a figure
    /// of no year.
    #[must_use]
    pub fn years(&self, key: &str) -> Vec<i32> {
        let mut out: Vec<i32> = self
            .fields
            .iter()
            .filter(|f| f.key == key)
            .filter_map(|f| f.year)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// An area's value of a field, where it is published.
    #[must_use]
    pub fn value(&self, area: usize, field: usize) -> Option<f32> {
        self.areas
            .get(area)?
            .values
            .get(field)
            .copied()
            .filter(|v| v.is_finite())
    }

    /// Carry `child`'s figures up into this set by each child's `parent`: a
    /// count is summed, a share or mean weighted ([`Roll`]). A figure this set
    /// already publishes for an area is kept; fields it lacks are added.
    pub fn roll_up(&mut self, child: &AreaSet) {
        let index: std::collections::HashMap<&str, usize> = self
            .areas
            .iter()
            .enumerate()
            .map(|(i, a)| (a.id.as_str(), i))
            .collect();
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); self.areas.len()];
        for (ci, c) in child.areas.iter().enumerate() {
            if let Some(&p) = c.parent.as_deref().and_then(|p| index.get(p)) {
                children[p].push(ci);
            }
        }
        for (cf, field) in child.fields.iter().enumerate() {
            // A series rolls up year by year: each year's share is weighted
            // by the same year's people.
            let weight = match &field.roll {
                Roll::Keep => continue,
                Roll::Sum => None,
                Roll::Mean { weight } => match child.field_at(weight, field.year) {
                    Some(w) => Some(w),
                    None => continue,
                },
            };
            let same = |f: &Field| f.key == field.key && f.year == field.year;
            let pf = match self.fields.iter().position(same) {
                Some(f) => f,
                None => {
                    self.fields.push(field.clone());
                    self.fields.len() - 1
                }
            };
            for (pi, kids) in children.iter().enumerate() {
                let area = &mut self.areas[pi];
                if area.values.len() <= pf {
                    area.values.resize(pf + 1, f32::NAN);
                }
                if area.values[pf].is_finite() {
                    continue;
                }
                let (mut num, mut den, mut any) = (0.0f64, 0.0f64, false);
                for &k in kids {
                    let Some(v) = child.value(k, cf) else {
                        continue;
                    };
                    match weight {
                        None => {
                            num += f64::from(v);
                            any = true;
                        }
                        Some(w) => {
                            if let Some(wv) = child.value(k, w) {
                                num += f64::from(v) * f64::from(wv);
                                den += f64::from(wv);
                                any = den > 0.0;
                            }
                        }
                    }
                }
                if any {
                    area.values[pf] = if weight.is_some() { num / den } else { num } as f32;
                }
            }
        }
        for a in &mut self.areas {
            a.values.resize(self.fields.len(), f32::NAN);
        }
    }

    /// The area a point lies in, if any (the first, where areas overlap).
    #[must_use]
    pub fn area_at(&self, p: LonLat) -> Option<usize> {
        let key = (p[0].floor() as i32, p[1].floor() as i32);
        self.grid
            .get(&key)?
            .iter()
            .map(|&i| i as usize)
            .find(|&i| self.areas[i].contains(p))
    }

    /// An area by its code.
    #[must_use]
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.areas.iter().position(|a| a.id == id)
    }

    /// The stored form: compact enough to ship (`docs/notes/scenarios.md`).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let q = |v: f64| (v / QUANTUM).round() as i32;
        let stored = StoredSet {
            format: AREA_FORMAT,
            name: self.name.clone(),
            attribution: self.attribution.clone(),
            rung: self.rung,
            fields: self.fields.clone(),
            areas: self
                .areas
                .iter()
                .map(|a| StoredArea {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    country: a.country.clone(),
                    polygons: a
                        .polygons
                        .iter()
                        .map(|rings| {
                            rings
                                .iter()
                                .map(|ring| {
                                    let mut prev = [0i32; 2];
                                    let mut out = Vec::with_capacity(ring.len() * 2);
                                    for p in ring {
                                        let c = [q(p[0]), q(p[1])];
                                        out.push(c[0] - prev[0]);
                                        out.push(c[1] - prev[1]);
                                        prev = c;
                                    }
                                    out
                                })
                                .collect()
                        })
                        .collect(),
                    parent: a.parent.clone(),
                    values: a.values.clone(),
                })
                .collect(),
        };
        let mut bytes = MAGIC.to_vec();
        bytes.extend(postcard::to_allocvec(&stored).unwrap_or_default());
        bytes
    }

    /// Read a stored set.
    pub fn decode(bytes: &[u8]) -> Result<AreaSet, AreaFileError> {
        let body = bytes
            .strip_prefix(MAGIC.as_slice())
            .ok_or(AreaFileError::NotAnAreaFile)?;
        // The format first, alone: an older file's layout would not decode.
        let (format, _) = postcard::take_from_bytes::<u32>(body)?;
        if format != AREA_FORMAT {
            return Err(AreaFileError::Format(format));
        }
        let stored: StoredSet = postcard::from_bytes(body)?;
        let areas = stored
            .areas
            .into_iter()
            .map(|a| {
                let polygons = a
                    .polygons
                    .into_iter()
                    .map(|rings| {
                        rings
                            .into_iter()
                            .map(|deltas| {
                                let mut acc = [0i64; 2];
                                deltas
                                    .chunks_exact(2)
                                    .map(|d| {
                                        acc[0] += i64::from(d[0]);
                                        acc[1] += i64::from(d[1]);
                                        [acc[0] as f64 * QUANTUM, acc[1] as f64 * QUANTUM]
                                    })
                                    .collect()
                            })
                            .collect()
                    })
                    .collect();
                let mut area = Area::new(a.id, a.name, a.country, polygons);
                area.parent = a.parent;
                area.values = a.values;
                area
            })
            .collect();
        Ok(AreaSet::new(stored.name, stored.attribution, areas)
            .with_rung(stored.rung)
            .with_fields(stored.fields))
    }
}

/// Douglas–Peucker in degrees: the ring with every point dropped that lies
/// within `tolerance` of the line its neighbours keep. A ring that would fall
/// under four points is kept as it is, so no small island vanishes.
#[must_use]
pub fn simplify_ring(ring: &[LonLat], tolerance: f64) -> Vec<LonLat> {
    if ring.len() <= 4 {
        return ring.to_vec();
    }
    let mut keep = vec![false; ring.len()];
    keep[0] = true;
    keep[ring.len() - 1] = true;
    let mut stack = vec![(0usize, ring.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (mut far, mut far_d) = (0, 0.0);
        for i in a + 1..b {
            let d = segment_distance(ring[i], ring[a], ring[b]);
            if d > far_d {
                far = i;
                far_d = d;
            }
        }
        if far_d > tolerance {
            keep[far] = true;
            stack.push((a, far));
            stack.push((far, b));
        }
    }
    let out: Vec<LonLat> = ring
        .iter()
        .zip(&keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect();
    if out.len() < 4 {
        ring.to_vec()
    } else {
        out
    }
}

fn segment_distance(p: LonLat, a: LonLat, b: LonLat) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    };
    (p[0] - (a[0] + t * dx)).hypot(p[1] - (a[1] + t * dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(id: &str, w: f64, s: f64, size: f64) -> Area {
        Area::new(
            id.into(),
            id.into(),
            "TST".into(),
            vec![vec![vec![
                [w, s],
                [w + size, s],
                [w + size, s + size],
                [w, s + size],
            ]]],
        )
    }

    #[test]
    fn boards_parse_and_print() {
        for s in ["areas:ne-admin1", "h3:5", "network:nl-rail"] {
            assert_eq!(s.parse::<Board>().unwrap().to_string(), s);
        }
        assert!("h3:16".parse::<Board>().is_err());
        assert!("grid:5".parse::<Board>().is_err());
    }

    #[test]
    fn a_hole_is_not_inside() {
        let mut a = square("donut", 0.0, 0.0, 4.0);
        a.polygons[0].push(vec![[1.0, 1.0], [3.0, 1.0], [3.0, 3.0], [1.0, 3.0]]);
        assert!(a.contains([0.5, 0.5]));
        assert!(!a.contains([2.0, 2.0]));
        assert!(!a.contains([5.0, 2.0]));
        let (v, i) = a.triangles();
        assert_eq!(v.len(), 8);
        assert_eq!(i.len() % 3, 0);
        assert!(!i.is_empty());
    }

    #[test]
    fn a_set_survives_its_file_to_the_metre() {
        let set = AreaSet::new(
            "test".into(),
            "nobody".into(),
            vec![
                square("a", 5.123_456, 52.0, 1.0),
                square("b", 6.123_456, 52.0, 1.0),
            ],
        );
        let back = AreaSet::decode(&set.encode()).unwrap();
        assert_eq!(back.name, "test");
        assert_eq!(back.areas.len(), 2);
        let p = back.areas[0].polygons[0][0][0];
        assert!((p[0] - 5.123_46).abs() < 1e-9, "{p:?}");
        assert_eq!(back.area_at([6.5, 52.5]), Some(1));
        assert_eq!(back.area_at([9.0, 52.5]), None);
        assert!(AreaSet::decode(b"nonsense").is_err());
    }

    #[test]
    fn simplifying_drops_points_on_a_line() {
        let ring: Vec<LonLat> = (0..=10)
            .map(|i| [f64::from(i), 0.0])
            .chain([[10.0, 5.0], [0.0, 5.0]])
            .collect();
        let s = simplify_ring(&ring, 0.01);
        assert_eq!(s.len(), 4, "{s:?}");
    }

    fn fields() -> Vec<Field> {
        vec![
            Field {
                key: "people".into(),
                label: "Inhabitants".into(),
                unit: "people".into(),
                roll: Roll::Sum,
                year: Some(2024),
            },
            Field {
                key: "old".into(),
                label: "Aged 65 and over".into(),
                unit: "%".into(),
                roll: Roll::Mean {
                    weight: "people".into(),
                },
                year: Some(2024),
            },
        ]
    }

    #[test]
    fn figures_roll_up_summed_and_weighted() {
        let mut kids = vec![square("a", 0.0, 0.0, 1.0), square("b", 1.0, 0.0, 1.0)];
        let mut hidden = square("c", 2.0, 0.0, 1.0);
        kids[0].values = vec![100.0, 10.0];
        kids[1].values = vec![300.0, 30.0];
        // Kept back: it adds nothing, rather than a zero.
        hidden.values = vec![f32::NAN, f32::NAN];
        kids.push(hidden);
        for k in &mut kids {
            k.parent = Some("P".into());
        }
        let child = AreaSet::new("kids".into(), "t".into(), kids)
            .with_rung(Rung::Neighbourhood)
            .with_fields(fields());
        let mut parent = AreaSet::new("p".into(), "t".into(), vec![square("P", 0.0, 0.0, 3.0)])
            .with_rung(Rung::District);
        parent.roll_up(&child);
        assert_eq!(parent.fields.len(), 2);
        assert_eq!(parent.value(0, 0), Some(400.0));
        assert_eq!(parent.value(0, 1), Some(25.0));
        // And it survives the file.
        let back = AreaSet::decode(&parent.encode()).unwrap();
        assert_eq!(back.rung, Rung::District);
        assert_eq!(back.value(0, 1), Some(25.0));
        assert_eq!(back.field("old"), Some(1));
    }

    #[test]
    fn a_series_rolls_up_year_by_year() {
        let f = |key: &str, year: i32, roll: Roll| Field {
            key: key.into(),
            label: key.into(),
            unit: String::new(),
            roll,
            year: Some(year),
        };
        let mean = || Roll::Mean {
            weight: "people".into(),
        };
        let fields = vec![
            f("people", 2014, Roll::Sum),
            f("score", 2014, mean()),
            f("people", 2015, Roll::Sum),
            f("score", 2015, mean()),
        ];
        let mut kids = vec![square("a", 0.0, 0.0, 1.0), square("b", 1.0, 0.0, 1.0)];
        kids[0].values = vec![100.0, 1.0, 300.0, 1.0];
        kids[1].values = vec![300.0, 0.0, 100.0, 0.0];
        for k in &mut kids {
            k.parent = Some("P".into());
        }
        let child = AreaSet::new("kids".into(), "t".into(), kids).with_fields(fields);
        let mut parent = AreaSet::new("p".into(), "t".into(), vec![square("P", 0.0, 0.0, 2.0)]);
        parent.roll_up(&child);
        assert_eq!(parent.years("score"), [2014, 2015]);
        let score = |y| parent.value(0, parent.field_at("score", Some(y)).unwrap());
        // Each year weighted by its own year's people.
        assert_eq!(score(2014), Some(0.25));
        assert_eq!(score(2015), Some(0.75));
        // Before the series, its first year; after it, its last.
        assert_eq!(score(2000), Some(0.25));
        assert_eq!(score(2050), Some(0.75));
        assert_eq!(
            parent.field_at("score", None),
            parent.field_at("score", Some(2015))
        );
    }

    #[test]
    fn an_inner_point_is_inside_a_crescent() {
        // A C shape whose bounding-box middle is outside it.
        let c = Area::new(
            "c".into(),
            "c".into(),
            "TST".into(),
            vec![vec![vec![
                [0.0, 0.0],
                [3.0, 0.0],
                [3.0, 1.0],
                [1.0, 1.0],
                [1.0, 2.0],
                [3.0, 2.0],
                [3.0, 3.0],
                [0.0, 3.0],
            ]]],
        );
        let p = c.inner_point().unwrap();
        assert!(c.contains(p), "{p:?}");
    }
}
