//! Place search and reverse geocoding, in place of Nominatim.
//!
//! The client asks two things of a geocoder (`docs/notes/the-server-stack.md`):
//! `/search?q=` for the search bar, route endpoints and the agent, and
//! `/reverse?lat=&lon=` for a picked point, a bookmark or a photo. Both are
//! answered here, at `/geocode/search` and `/geocode/reverse`, in the shape of
//! Nominatim's `jsonv2` the client already parses (`display_name`, `lat`/`lon`
//! as strings, `name`, `addresstype`), so pointing the server's `geocoder_url`
//! at `…/geocode` is the whole switch.
//!
//! **The index is built from the OpenStreetMap extract** by the `geocoder`
//! dataset ([`run`]): every address (Dutch OSM carries the BAG's), grouped by
//! street and town; every named place (city to neighbourhood); and every named
//! amenity, shop, station and sight. It is one file, `<data_dir>/geocoder/index.bin`,
//! which the service reads at start and again whenever a run replaces it.
//!
//! **Search is by word prefix**, so a query matches while it is typed — what
//! Nominatim could not do (`docs/notes/geocoding-latency.md`). Every word of the
//! query must begin a word of the result's name or town; a number is a house
//! number on a street that matched, a `1234 AB` a postcode.
//!
//! Data: © OpenStreetMap contributors, ODbL.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::osm::{self, tag, Bbox, Node, TagView, Wanted, Way};
use crate::pipeline::{Count, Ctx, DatasetResult, Outcome, PipelineError, Progress};

/// Where the index lives under atlas's data directory.
pub const INDEX_FILE: &str = "geocoder/index.bin";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Place {
    pub name: String,
    /// The town it lies in, when that is not the place itself.
    pub town: String,
    /// What it is, Nominatim's `addresstype`: `city`, `village`, `road`,
    /// `restaurant`, …
    pub kind: String,
    pub lat: f32,
    pub lon: f32,
    /// Higher sorts first among equal matches: a city before a street of the
    /// same name, a street before a café.
    pub rank: u8,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Number {
    pub number: String,
    pub postcode: String,
    pub lat: f32,
    pub lon: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Street {
    pub name: String,
    pub town: String,
    pub lat: f32,
    pub lon: f32,
    pub numbers: Vec<Number>,
}

/// What the dataset writes and the service reads.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub places: Vec<Place>,
    pub streets: Vec<Street>,
}

/// Lower-case, accents folded, everything but letters and digits a space.
#[must_use]
pub fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            'ÿ' => 'y',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        };
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ─── Searching ───────────────────────────────────────────────────────────────

/// The index with what it is searched by. Documents are numbered places first,
/// then streets.
pub struct Searcher {
    index: Index,
    /// Every word of a document's name and town → the documents, for prefix
    /// ranges.
    words: BTreeMap<String, Vec<u32>>,
    /// House numbers by [`GRID_DEG`] cell, as (street, number) — the reverse
    /// lookup's neighbourhood.
    grid: HashMap<(i32, i32), Vec<(u32, u32)>>,
    /// Places by the same cells, for a point with no address near it.
    place_grid: HashMap<(i32, i32), Vec<u32>>,
    /// House numbers by postcode (`9712hn`), for a query that is one.
    postcodes: HashMap<String, Vec<(u32, u32)>>,
}

/// The reverse lookup's cell: about 220 m north–south, 130 m east–west here.
const GRID_DEG: f32 = 0.002;
/// How far a reverse lookup takes the nearest address from.
const REVERSE_ADDRESS_M: f32 = 120.0;

fn cell(lat: f32, lon: f32) -> (i32, i32) {
    (
        (lat / GRID_DEG).floor() as i32,
        (lon / GRID_DEG).floor() as i32,
    )
}

fn metres(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dy = (a.0 - b.0) * 111_320.0;
    let dx = (a.1 - b.1) * 111_320.0 * a.0.to_radians().cos();
    dx.hypot(dy)
}

/// One answer, before it is written as Nominatim's JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub name: String,
    pub display: String,
    pub kind: String,
    pub lat: f32,
    pub lon: f32,
}

impl Hit {
    /// Nominatim's `address` parts, which the web UI names a place from
    /// (`crates/atlas/web/src/lib/calendar.ts`, `stats.ts`), read back from the
    /// display this module wrote: `Name, Town` or `Street 1, 1234AB Town`.
    fn address(&self) -> Value {
        let (_, rest) = self.display.split_once(", ").unwrap_or((&self.display, ""));
        let mut a = serde_json::Map::new();
        let mut put = |k: &str, v: &str| {
            if !v.is_empty() {
                a.insert(k.into(), Value::String(v.into()));
            }
        };
        let town = match rest.split_once(' ') {
            Some((pc, town)) if pc.len() == 6 && pc[..4].bytes().all(|b| b.is_ascii_digit()) => {
                put("postcode", pc);
                town
            }
            _ => rest,
        };
        match self.kind.as_str() {
            "city" | "town" | "village" => put(&self.kind, &self.name),
            "house" => {
                let (road, number) = self.name.rsplit_once(' ').unwrap_or((&self.name, ""));
                put("road", road);
                put("house_number", number);
                put("city", town);
            }
            "road" | "postcode" => {
                put("road", &self.name);
                put("city", town);
            }
            _ => put("city", town),
        }
        Value::Object(a)
    }

    fn json(&self) -> Value {
        json!({
            "display_name": self.display,
            "name": self.name,
            "lat": format!("{:.7}", self.lat),
            "lon": format!("{:.7}", self.lon),
            "addresstype": self.kind,
            "type": self.kind,
            "address": self.address(),
        })
    }
}

fn joined(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(", ")
}

/// A Dutch postcode's two halves, `1234` and `ab`.
fn postcode_at(tokens: &[&str], i: usize) -> Option<String> {
    let digits = tokens.get(i)?;
    if digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit()) {
        let letters = tokens.get(i + 1)?;
        if letters.len() == 2 && letters.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Some(format!("{digits}{letters}"));
        }
    }
    None
}

impl Searcher {
    #[must_use]
    pub fn new(index: Index) -> Self {
        let mut words: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        let mut add = |doc: u32, text: &str| {
            for w in normalise(text).split(' ').filter(|w| !w.is_empty()) {
                let ids = words.entry(w.to_string()).or_default();
                if ids.last() != Some(&doc) {
                    ids.push(doc);
                }
            }
        };
        let n_places = index.places.len() as u32;
        for (i, p) in index.places.iter().enumerate() {
            add(i as u32, &p.name);
            add(i as u32, &p.town);
            // What it is counts as a word too: "station groningen" is the
            // station named Groningen.
            add(i as u32, &p.kind.replace('_', " "));
        }
        for (i, s) in index.streets.iter().enumerate() {
            add(n_places + i as u32, &s.name);
            add(n_places + i as u32, &s.town);
        }
        let mut grid: HashMap<(i32, i32), Vec<(u32, u32)>> = HashMap::new();
        for (si, s) in index.streets.iter().enumerate() {
            for (ni, n) in s.numbers.iter().enumerate() {
                grid.entry(cell(n.lat, n.lon))
                    .or_default()
                    .push((si as u32, ni as u32));
            }
        }
        let mut postcodes: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
        for (si, s) in index.streets.iter().enumerate() {
            for (ni, n) in s.numbers.iter().enumerate() {
                let pc = normalise(&n.postcode).replace(' ', "");
                if !pc.is_empty() {
                    postcodes
                        .entry(pc)
                        .or_default()
                        .push((si as u32, ni as u32));
                }
            }
        }
        let mut place_grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (i, p) in index.places.iter().enumerate() {
            if p.rank >= RANK_HAMLET {
                place_grid
                    .entry(cell(p.lat, p.lon))
                    .or_default()
                    .push(i as u32);
            }
        }
        Self {
            index,
            words,
            grid,
            place_grid,
            postcodes,
        }
    }

    /// A query that is a postcode, with or without a number: the street (or the
    /// house) it names.
    fn by_postcode(&self, postcode: &str, number: Option<&str>, limit: usize) -> Vec<Hit> {
        let mut out: Vec<Hit> = Vec::new();
        for &(si, ni) in self.postcodes.get(postcode).into_iter().flatten() {
            let s = &self.index.streets[si as usize];
            let n = &s.numbers[ni as usize];
            let town = joined(&[&n.postcode, &s.town]).replace(", ", " ");
            let hit = match number {
                Some(w) if normalise(&n.number).replace(' ', "") != w => continue,
                Some(_) => {
                    let name = format!("{} {}", s.name, n.number);
                    Hit {
                        display: joined(&[&name, &town]),
                        name,
                        kind: "house".into(),
                        lat: n.lat,
                        lon: n.lon,
                    }
                }
                None => Hit {
                    name: s.name.clone(),
                    display: joined(&[&s.name, &town]),
                    kind: "postcode".into(),
                    lat: n.lat,
                    lon: n.lon,
                },
            };
            if !out.iter().any(|h| h.display == hit.display) {
                out.push(hit);
            }
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    /// The documents with a word that begins with `prefix`.
    fn prefixed(&self, prefix: &str) -> HashSet<u32> {
        self.words
            .range(prefix.to_string()..)
            .take_while(|(w, _)| w.starts_with(prefix))
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect()
    }

    /// Up to `limit` answers to `query`, the nearest to `near` first among
    /// otherwise equal ones.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize, near: Option<(f32, f32)>) -> Vec<Hit> {
        let norm = normalise(query);
        let tokens: Vec<&str> = norm.split(' ').filter(|t| !t.is_empty()).collect();
        let (mut words, mut number, mut postcode) = (Vec::new(), None, None);
        let mut i = 0;
        while i < tokens.len() {
            if let Some(pc) = postcode_at(&tokens, i) {
                postcode = Some(pc);
                i += 2;
                continue;
            }
            let t = tokens[i];
            if t.starts_with(|c: char| c.is_ascii_digit()) && number.is_none() {
                number = Some(t.to_string());
            } else {
                words.push(t);
            }
            i += 1;
        }
        if words.is_empty() {
            return postcode
                .map(|pc| self.by_postcode(&pc, number.as_deref(), limit))
                .unwrap_or_default();
        }
        // Rarest word first, so the intersection starts small.
        let mut sets: Vec<HashSet<u32>> = words.iter().map(|w| self.prefixed(w)).collect();
        sets.sort_by_key(HashSet::len);
        let mut docs = sets.remove(0);
        for s in &sets {
            docs.retain(|d| s.contains(d));
        }
        let n_places = self.index.places.len() as u32;
        let whole = words.join(" ");
        // A word must begin a word of the *name*: "groningen" alone is the city,
        // not every street whose town is Groningen.
        let names_it = |name: &str, kind: &str| {
            let n = normalise(&format!("{name} {}", kind.replace('_', " ")));
            words
                .iter()
                .any(|w| n.split(' ').any(|nw| nw.starts_with(w)))
        };
        let mut hits: Vec<(f32, Hit)> = Vec::new();
        for doc in docs {
            let (name, kind) = if doc < n_places {
                let p = &self.index.places[doc as usize];
                (&p.name, p.kind.as_str())
            } else {
                (&self.index.streets[(doc - n_places) as usize].name, "")
            };
            if !names_it(name, kind) {
                continue;
            }
            if doc < n_places {
                if number.is_some() || postcode.is_some() {
                    continue;
                }
                let p = &self.index.places[doc as usize];
                let hit = Hit {
                    name: p.name.clone(),
                    display: joined(&[&p.name, &p.town]),
                    kind: p.kind.clone(),
                    lat: p.lat,
                    lon: p.lon,
                };
                hits.push((score(p.rank, &p.name, &whole, near, (p.lat, p.lon)), hit));
                continue;
            }
            let s = &self.index.streets[(doc - n_places) as usize];
            let wanted = |n: &Number| {
                number
                    .as_deref()
                    .is_none_or(|w| normalise(&n.number).replace(' ', "") == w)
                    && postcode
                        .as_deref()
                        .is_none_or(|pc| normalise(&n.postcode).replace(' ', "") == pc)
            };
            if number.is_none() && postcode.is_none() {
                let hit = Hit {
                    name: s.name.clone(),
                    display: joined(&[&s.name, &s.town]),
                    kind: "road".into(),
                    lat: s.lat,
                    lon: s.lon,
                };
                hits.push((
                    score(RANK_STREET, &s.name, &whole, near, (s.lat, s.lon)),
                    hit,
                ));
            } else if let Some(n) = s.numbers.iter().find(|n| wanted(n)) {
                let name = format!("{} {}", s.name, n.number);
                let town = joined(&[&n.postcode, &s.town]).replace(", ", " ");
                let hit = Hit {
                    display: joined(&[&name, &town]),
                    name,
                    kind: "house".into(),
                    lat: n.lat,
                    lon: n.lon,
                };
                hits.push((
                    score(RANK_HOUSE, &s.name, &whole, near, (n.lat, n.lon)),
                    hit,
                ));
            }
        }
        hits.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| a.1.display.cmp(&b.1.display))
        });
        let mut seen = HashSet::new();
        hits.into_iter()
            .map(|(_, h)| h)
            .filter(|h| seen.insert(h.display.clone()))
            .take(limit)
            .collect()
    }

    /// The address at a point: the nearest house number within
    /// [`REVERSE_ADDRESS_M`], else the nearest town.
    #[must_use]
    pub fn reverse(&self, lat: f32, lon: f32) -> Option<Hit> {
        let (cy, cx) = cell(lat, lon);
        let mut best: Option<(f32, u32, u32)> = None;
        for dy in -1..=1 {
            for dx in -1..=1 {
                for &(si, ni) in self.grid.get(&(cy + dy, cx + dx)).into_iter().flatten() {
                    let n = &self.index.streets[si as usize].numbers[ni as usize];
                    let d = metres((lat, lon), (n.lat, n.lon));
                    if best.is_none_or(|b| d < b.0) {
                        best = Some((d, si, ni));
                    }
                }
            }
        }
        if let Some((d, si, ni)) = best.filter(|b| b.0 <= REVERSE_ADDRESS_M) {
            let _ = d;
            let s = &self.index.streets[si as usize];
            let n = &s.numbers[ni as usize];
            let name = format!("{} {}", s.name, n.number);
            let town = joined(&[&n.postcode, &s.town]).replace(", ", " ");
            return Some(Hit {
                display: joined(&[&name, &town]),
                name,
                kind: "house".into(),
                lat: n.lat,
                lon: n.lon,
            });
        }
        // No address: the nearest place, looked for in widening rings.
        for r in [2, 10, 40] {
            let mut near: Option<(f32, u32)> = None;
            for dy in -r..=r {
                for dx in -r..=r {
                    for &pi in self
                        .place_grid
                        .get(&(cy + dy, cx + dx))
                        .into_iter()
                        .flatten()
                    {
                        let p = &self.index.places[pi as usize];
                        let d = metres((lat, lon), (p.lat, p.lon));
                        if near.is_none_or(|b| d < b.0) {
                            near = Some((d, pi));
                        }
                    }
                }
            }
            if let Some((_, pi)) = near {
                let p = &self.index.places[pi as usize];
                return Some(Hit {
                    name: p.name.clone(),
                    display: joined(&[&p.name, &p.town]),
                    kind: p.kind.clone(),
                    lat: p.lat,
                    lon: p.lon,
                });
            }
        }
        None
    }
}

/// How well a match answers the query: its rank, a name that is the query
/// itself, a short name, and nearness to where the asker is.
fn score(rank: u8, name: &str, whole: &str, near: Option<(f32, f32)>, at: (f32, f32)) -> f32 {
    let n = normalise(name);
    let mut s = f32::from(rank);
    if n == whole {
        s += 12.0;
    } else if n.starts_with(whole) {
        s += 6.0;
    }
    s -= n.len() as f32 * 0.1;
    if let Some(near) = near {
        // A kilometre costs a little, a hundred a lot: nearby wins a tie, a
        // city still beats a café of the same name across the country.
        s -= (metres(near, at) / 1000.0 + 1.0).ln() * 6.0;
    }
    s
}

// ─── Ranks ───────────────────────────────────────────────────────────────────

const RANK_HOUSE: u8 = 30;
const RANK_STREET: u8 = 32;
const RANK_HAMLET: u8 = 40;

fn place_rank(kind: &str) -> Option<u8> {
    Some(match kind {
        "city" => 80,
        "town" => 70,
        "village" => 60,
        "suburb" | "borough" => 50,
        "hamlet" | "island" => RANK_HAMLET,
        "quarter" | "neighbourhood" => 36,
        _ => return None,
    })
}

/// The keys a named thing is found by, and what each makes it.
const POI_KEYS: &[&str] = &[
    "amenity", "shop", "tourism", "leisure", "historic", "railway", "aeroway", "office", "craft",
];

/// A station or an airport is what a person typing its name means, before a
/// street named after it.
fn poi_rank(kind: &str) -> u8 {
    match kind {
        "aerodrome" => 50,
        "station" | "halt" => 45,
        _ => 20,
    }
}

fn poi_kind(tags: &TagView<'_>) -> Option<String> {
    if tag(tags, "railway").is_some_and(|r| r != "station" && r != "halt")
        || tag(tags, "aeroway").is_some_and(|a| a != "aerodrome")
    {
        return None;
    }
    POI_KEYS
        .iter()
        .find_map(|k| tag(tags, k).filter(|v| *v != "yes").map(str::to_string))
}

// ─── Building ────────────────────────────────────────────────────────────────

fn is_address(tags: &TagView<'_>) -> bool {
    tag(tags, "addr:housenumber").is_some() && tag(tags, "addr:street").is_some()
}

fn is_place(tags: &TagView<'_>) -> bool {
    tag(tags, "name").is_some() && tag(tags, "place").is_some_and(|p| place_rank(p).is_some())
}

fn is_poi(tags: &TagView<'_>) -> bool {
    tag(tags, "name").is_some() && poi_kind(tags).is_some()
}

fn wanted_node(tags: &TagView<'_>) -> bool {
    is_address(tags) || is_place(tags) || is_poi(tags)
}

fn wanted_way(tags: &TagView<'_>) -> bool {
    is_address(tags) || is_poi(tags)
}

const WANTED: Wanted = Wanted {
    node: wanted_node,
    way: wanted_way,
    ..Wanted::NOTHING
};

/// What the passes gather before the towns are known.
#[derive(Default)]
struct Gathered {
    seen: HashSet<(bool, i64)>,
    places: Vec<Place>,
    /// (street, town) → numbers.
    streets: HashMap<(String, String), Vec<Number>>,
}

impl Gathered {
    fn take(&mut self, is_way: bool, id: i64, tags: &crate::osm::Tags, at: (f64, f64)) {
        if !self.seen.insert((is_way, id)) {
            return;
        }
        let view = crate::osm::layers::view(tags);
        let (lat, lon) = (at.0 as f32, at.1 as f32);
        if is_address(&view) {
            let street = tag(&view, "addr:street").unwrap_or_default().to_string();
            let town = tag(&view, "addr:city").unwrap_or_default().to_string();
            self.streets
                .entry((street, town))
                .or_default()
                .push(Number {
                    number: format!(
                        "{}{}",
                        tag(&view, "addr:housenumber").unwrap_or_default(),
                        tag(&view, "addr:unit")
                            .map(|u| format!("-{u}"))
                            .unwrap_or_default()
                    ),
                    postcode: tag(&view, "addr:postcode").unwrap_or_default().to_string(),
                    lat,
                    lon,
                });
        }
        let Some(name) = tag(&view, "name") else {
            return;
        };
        if !is_way {
            if let Some(rank) = tag(&view, "place").and_then(place_rank) {
                self.places.push(Place {
                    name: name.to_string(),
                    town: String::new(),
                    kind: tag(&view, "place").unwrap_or_default().to_string(),
                    lat,
                    lon,
                    rank,
                });
                return;
            }
        }
        if let Some(kind) = poi_kind(&view) {
            self.places.push(Place {
                name: name.to_string(),
                town: tag(&view, "addr:city").unwrap_or_default().to_string(),
                rank: poi_rank(&kind),
                kind,
                lat,
                lon,
            });
        }
    }

    fn take_node(&mut self, n: &Node) {
        self.take(false, n.id, &n.tags, (n.lat, n.lon));
    }

    fn take_way(&mut self, w: &Way) {
        if let Some(c) = w.center() {
            self.take(true, w.id, &w.tags, c);
        }
    }

    /// The index, with a town given to every place and street that lacks one:
    /// the nearest city, town or village.
    fn finish(self) -> Index {
        let Gathered {
            mut places,
            streets,
            ..
        } = self;
        let towns: Vec<(String, f32, f32)> = places
            .iter()
            .filter(|p| p.rank >= 60)
            .map(|p| (p.name.clone(), p.lat, p.lon))
            .collect();
        let nearest_town = |lat: f32, lon: f32| -> String {
            towns
                .iter()
                .min_by(|a, b| {
                    metres((lat, lon), (a.1, a.2)).total_cmp(&metres((lat, lon), (b.1, b.2)))
                })
                .map(|t| t.0.clone())
                .unwrap_or_default()
        };
        for p in &mut places {
            if p.town.is_empty() && p.rank < 60 {
                p.town = nearest_town(p.lat, p.lon);
                if p.town == p.name {
                    p.town.clear();
                }
            }
        }
        let mut out: Vec<Street> = streets
            .into_iter()
            .map(|((name, town), mut numbers)| {
                numbers.sort_by(|a, b| a.number.cmp(&b.number));
                let n = numbers.len() as f32;
                let lat = numbers.iter().map(|x| x.lat).sum::<f32>() / n;
                let lon = numbers.iter().map(|x| x.lon).sum::<f32>() / n;
                let town = if town.is_empty() {
                    nearest_town(lat, lon)
                } else {
                    town
                };
                Street {
                    name,
                    town,
                    lat,
                    lon,
                    numbers,
                }
            })
            .collect();
        out.sort_by(|a, b| (&a.town, &a.name).cmp(&(&b.town, &b.name)));
        Index {
            places,
            streets: out,
        }
    }
}

/// The `geocoder` dataset: index the run's area (the country, for the one the
/// service answers from: `--name nederland --bbox 13000 306000 278000 620000`)
/// and replace `<data_dir>/geocoder/index.bin`.
pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let path = osm::ensure_extract(ctx)?;
    let cells = osm::area_cells(ctx.bbox, 9, 0);
    let mut gathered = Gathered::default();
    for (done, &(x, y)) in cells.iter().enumerate() {
        let area = Bbox::tile(x, y, 9);
        let store = osm::load(ctx, &path, area, &WANTED)?;
        for n in &store.nodes {
            gathered.take_node(n);
        }
        for w in &store.ways {
            gathered.take_way(w);
        }
        ctx.progress(Progress {
            phase: Some("stukken".into()),
            done: Some(done as u64 + 1),
            total: Some(Some(cells.len() as u64)),
        });
    }
    let index = gathered.finish();
    let numbers: usize = index.streets.iter().map(|s| s.numbers.len()).sum();
    ctx.log(format!(
        "{} plaatsen, {} straten, {numbers} adressen",
        index.places.len(),
        index.streets.len()
    ));
    if index.places.is_empty() && index.streets.is_empty() {
        return Err(PipelineError::Failed("nothing to index in the area".into()));
    }
    let body = postcard::to_allocvec(&index).map_err(|e| PipelineError::Failed(e.to_string()))?;
    let file = ctx.data_dir()?.join(INDEX_FILE);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| PipelineError::Failed(e.to_string()))?;
    }
    let part = file.with_extension("part");
    std::fs::write(&part, &body)
        .and_then(|()| std::fs::rename(&part, &file))
        .map_err(|e| PipelineError::Failed(format!("{}: {e}", file.display())))?;
    ctx.log(format!("{} MB geschreven", body.len() / 1_000_000));
    // The web UI's Data tab reads each dataset's manifest to say what it covers.
    ctx.write_coverage(
        "geocoder/coverage.json",
        &cartopolis_geo::formats::coverage::Manifest::new(
            9,
            crate::pipeline::tile::sorted_cells(cells.iter().copied()),
        )
        .described(
            ctx.name.as_str(),
            "© OpenStreetMap contributors (ODbL)",
            "OpenStreetMap extract (Geofabrik)",
            ctx.bbox,
        )
        .with("address_count", numbers),
    )?;
    Ok(DatasetResult {
        cells: cells.len(),
        counts: vec![
            Count::new("places", index.places.len()),
            Count::new("addresses", numbers),
        ],
    })
}

// ─── Serving ─────────────────────────────────────────────────────────────────

/// The index the service answers from, re-read when a run replaces the file.
pub struct Geocoder {
    file: PathBuf,
    loaded: RwLock<Option<(SystemTime, Arc<Searcher>)>>,
}

impl Geocoder {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            file: data_dir.join(INDEX_FILE),
            loaded: RwLock::new(None),
        }
    }

    /// The current searcher, loading or reloading the file when it changed.
    /// `None` when there is no index yet.
    pub fn searcher(&self) -> Option<Arc<Searcher>> {
        let modified = std::fs::metadata(&self.file)
            .and_then(|m| m.modified())
            .ok()?;
        if let Some((at, s)) = self.loaded.read().ok()?.as_ref() {
            if *at == modified {
                return Some(s.clone());
            }
        }
        let bytes = std::fs::read(&self.file).ok()?;
        let index: Index = match postcard::from_bytes(&bytes) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(file = %self.file.display(), "unreadable geocoder index: {e}");
                return None;
            }
        };
        let searcher = Arc::new(Searcher::new(index));
        tracing::info!(file = %self.file.display(), "geocoder index loaded");
        if let Ok(mut slot) = self.loaded.write() {
            *slot = Some((modified, searcher.clone()));
        }
        Some(searcher)
    }
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    limit: Option<usize>,
    /// Nominatim's `west,north,east,south`: a bias here, never a bound.
    viewbox: Option<String>,
}

#[derive(Deserialize)]
pub struct ReverseQuery {
    lat: f32,
    lon: f32,
}

fn viewbox_centre(text: &str) -> Option<(f32, f32)> {
    let v: Vec<f32> = text
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    let [w, n, e, s] = v.as_slice() else {
        return None;
    };
    Some(((n + s) / 2.0, (w + e) / 2.0))
}

/// `GET /geocode/search?q=…&limit=…&viewbox=…` (`format` is ignored: always
/// `jsonv2`). Runs on a blocking thread: a first call loads the index.
pub async fn search(State(g): State<Arc<Geocoder>>, Query(q): Query<SearchQuery>) -> Json<Value> {
    let text = q.q.unwrap_or_default();
    let limit = q.limit.unwrap_or(10).clamp(1, 50);
    let near = q.viewbox.as_deref().and_then(viewbox_centre);
    let hits = tokio::task::spawn_blocking(move || {
        g.searcher()
            .map(|s| s.search(&text, limit, near))
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    Json(Value::Array(hits.iter().map(Hit::json).collect()))
}

/// `GET /geocode/reverse?lat=…&lon=…`: Nominatim's `{"error": …}` when
/// nothing is near.
pub async fn reverse(State(g): State<Arc<Geocoder>>, Query(q): Query<ReverseQuery>) -> Json<Value> {
    let hit =
        tokio::task::spawn_blocking(move || g.searcher().and_then(|s| s.reverse(q.lat, q.lon)))
            .await
            .ok()
            .flatten();
    Json(hit.map_or_else(|| json!({"error": "Unable to geocode"}), |h| h.json()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn searcher() -> Searcher {
        let n = |number: &str, lat, lon| Number {
            number: number.into(),
            postcode: "9712 HN".into(),
            lat,
            lon,
        };
        Searcher::new(Index {
            places: vec![
                Place {
                    name: "Groningen".into(),
                    kind: "city".into(),
                    lat: 53.2194,
                    lon: 6.5665,
                    rank: 80,
                    ..Place::default()
                },
                Place {
                    name: "Café De Markt".into(),
                    town: "Groningen".into(),
                    kind: "cafe".into(),
                    lat: 53.2190,
                    lon: 6.5670,
                    rank: 20,
                },
                Place {
                    name: "Groningen".into(),
                    town: "Zuidwolde".into(),
                    kind: "hamlet".into(),
                    lat: 52.66,
                    lon: 6.43,
                    rank: RANK_HAMLET,
                },
            ],
            streets: vec![Street {
                name: "Grote Markt".into(),
                town: "Groningen".into(),
                lat: 53.2193,
                lon: 6.5668,
                numbers: vec![n("1", 53.2192, 6.5667), n("2a", 53.2195, 6.5671)],
            }],
        })
    }

    #[test]
    fn a_word_matches_while_it_is_typed() {
        let s = searcher();
        let hits = s.search("grote ma", 5, None);
        assert_eq!(hits[0].display, "Grote Markt, Groningen");
        assert_eq!(hits[0].kind, "road");
        assert!(s
            .search("markt groningen", 5, None)
            .iter()
            .any(|h| h.name == "Grote Markt"));
    }

    #[test]
    fn the_city_comes_before_the_hamlet_of_the_same_name() {
        let hits = searcher().search("Groningen", 5, None);
        assert_eq!(hits[0].kind, "city");
        assert!(hits.iter().any(|h| h.display == "Groningen, Zuidwolde"));
    }

    #[test]
    fn a_number_and_a_postcode_find_the_house() {
        let s = searcher();
        let hits = s.search("Grote Markt 2A, Groningen", 5, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].display, "Grote Markt 2a, 9712 HN Groningen");
        assert_eq!(
            s.search("grote markt 1 9712 hn", 5, None)[0].name,
            "Grote Markt 1"
        );
        assert!(s.search("grote markt 7", 5, None).is_empty());
    }

    #[test]
    fn accents_and_case_do_not_matter() {
        assert_eq!(
            searcher().search("cafe de m", 5, None)[0].name,
            "Café De Markt"
        );
    }

    #[test]
    fn reverse_takes_the_nearest_house_or_else_the_town() {
        let s = searcher();
        let hit = s.reverse(53.21921, 6.56671).unwrap();
        assert_eq!(hit.name, "Grote Markt 1");
        let far = s.reverse(53.25, 6.60).unwrap();
        assert_eq!(far.name, "Groningen");
    }

    #[test]
    fn the_json_is_nominatim_s() {
        let v = searcher().search("groningen", 1, None)[0].json();
        // A string, as Nominatim writes it; f32 holds it to ~0.1 m.
        let lat: f64 = v["lat"].as_str().unwrap().parse().unwrap();
        assert!((lat - 53.2194).abs() < 1e-5, "{lat}");
        assert_eq!(v["addresstype"], "city");
        assert!(v["display_name"].is_string());
        assert_eq!(v["address"]["city"], "Groningen");
        let house = Hit {
            name: "Grote Markt 2a".into(),
            display: "Grote Markt 2a, 9712HN Groningen".into(),
            kind: "house".into(),
            lat: 0.0,
            lon: 0.0,
        }
        .json();
        assert_eq!(
            house["address"],
            json!({"road": "Grote Markt", "house_number": "2a", "postcode": "9712HN", "city": "Groningen"})
        );
    }

    #[test]
    fn the_index_survives_the_file() {
        let index = Index {
            places: searcher().index.places.clone(),
            streets: Vec::new(),
        };
        let bytes = postcard::to_allocvec(&index).unwrap();
        let back: Index = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back.places, index.places);
    }
}

/// Asks a built index the queries the client sends, with timings: a check by
/// hand against a whole-country index, not a test.
///
/// ```text
/// ATLAS_GEOCODER_INDEX=…/geocoder/index.bin cargo test --release -p cartopolis_atlas \
///   asks_a_built_index -- --ignored --nocapture
/// ```
#[cfg(test)]
mod built {
    use super::*;

    #[test]
    #[ignore = "needs ATLAS_GEOCODER_INDEX, a built whole-country index"]
    fn asks_a_built_index() {
        let path = std::env::var("ATLAS_GEOCODER_INDEX").unwrap();
        let t = std::time::Instant::now();
        let index: Index = postcard::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
        let s = Searcher::new(index);
        println!("loaded in {:?}", t.elapsed());
        for q in [
            "Groningen",
            "grote markt groningen",
            "Grote Markt 1 Groningen",
            "Damrak",
            "Amsterdam Centraal",
            "9712 HN",
            "Vismarkt",
            "Martinitoren",
            "Utrecht",
            "Hoogkerk",
            "Oude Ebbingestraat 32",
            "Zuiderdiep",
            "Rijksmuseum",
            "Schiphol",
            "den haag",
            "Albert Heijn Groningen",
            "station groningen",
            "Maastricht vrijthof",
            "zuiderdiep groningen",
            "Zuiderdiep 1",
        ] {
            let t = std::time::Instant::now();
            let hits = s.search(q, 3, Some((53.2194, 6.5665)));
            let names: Vec<String> = hits
                .iter()
                .map(|h| format!("{} [{}]", h.display, h.kind))
                .collect();
            println!("{q:30} {:>6.1?}  {}", t.elapsed(), names.join(" | "));
        }
        for (lat, lon) in [
            (53.2194, 6.5665),
            (52.3731, 4.8926),
            (53.10, 6.10),
            (52.0, 3.5),
        ] {
            let t = std::time::Instant::now();
            let hit = s.reverse(lat, lon);
            println!(
                "reverse {lat},{lon} {:>6.1?}  {:?}",
                t.elapsed(),
                hit.map(|h| h.display)
            );
        }
    }
}
