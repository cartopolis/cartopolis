//! **Registers**: the street objects a *city* surveys where the national register
//! is silent, and the public art Wikidata registers — per z14 cell, for the
//! client's `systems::municipal`.
//!
//! The client used to ask Gemeente Groningen's GeoServer for these itself, one
//! request per source per cell, and carried the city's layer names, field names
//! and vocabulary in its own code. Atlas asks instead, **once per layer per
//! run**: the server answers a whole layer over the city in one request (all
//! 126,074 trees in ~2 s, 2026-10-19), where the client's per-cell path was
//! nine requests for every cell it streamed. The client now reads one file per
//! cell from our own host, in `cartopolis_geo::formats::registers`, and knows
//! nothing about any city.
//!
//! A city's vocabulary is still read into the client's shapes **here, at the
//! parser**, for the reason `docs/notes/municipal-registers.md` gives: a tree with
//! a height, a prop with a `FurnitureKind` and a parking bay are what the client
//! can draw, and whatever a city knows beyond that is dropped where its meaning is
//! still understood.
//!
//! Every cell in a run's area is written, even an empty one, because each cell
//! says which kinds its registers are the authority on (`supplies`) — and a
//! covered cell with no lamp posts in it is the city's word that there are none.
//!
//! Data: Gemeente Groningen open data (CC BY 4.0); Wikidata (CC0); OpenStreetMap
//! through our own Overpass, only to see which statues the map already has.
//!
//! **Every source is asked once per run** — a layer, a query — and through the
//! source cache, so a re-run within a month asks nobody anything.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use cartopolis_core::world::tile_to_lat_lng;
use cartopolis_geo::ahn::wgs84_to_rd;
use cartopolis_geo::formats::coverage::Manifest;
use cartopolis_geo::formats::registers::{Cell as CellDoc, Prop, Tree, SUPPLIES_TREES};
use cartopolis_geo::furniture::FurnitureKind;
use serde_json::Value;

use super::geom::{polygon_rings, round};
use super::http::{get_json, query};
use super::rd::{rd_to_lat_lng, wgs84_bbox};
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{Count, Ctx, DatasetResult, Outcome, PipelineError};

// ─── What a source reads ─────────────────────────────────────────────────────

/// A feature's properties, as a reader sees them.
pub struct Fields<'a>(&'a Value);

impl Fields<'_> {
    #[must_use]
    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }
}

/// What one of a city's layers becomes. A reader may refuse a feature: a ground
/// spot in the lighting layer drawn as a four-metre column is worse than not
/// drawing it, so [`Yields::Props`] answers `None` for those.
pub enum Yields {
    Trees {
        height: fn(&Fields) -> Option<f32>,
    },
    /// The form, and an sRGB body colour where the register says one.
    Props {
        kind: fn(&Fields) -> Option<(FurnitureKind, Option<u32>)>,
    },
    /// Polygons, one parking bay each.
    Bays,
}

/// A kind of content a source is the authority on, over the cells it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Supply {
    Trees,
    Prop(FurnitureKind),
}

impl Supply {
    fn token(self) -> &'static str {
        match self {
            Self::Trees => SUPPLIES_TREES,
            Self::Prop(kind) => kind.slug(),
        }
    }
}

/// One layer of a city's register.
pub struct Source {
    /// Log name and count label.
    pub id: &'static str,
    /// The layer, as the city's service spells it.
    pub layer: &'static str,
    /// The geometry property — `GEOM` on most of Groningen's layers, `geom` on
    /// the parking bays, and the server refuses a `propertyName` it does not have.
    pub geometry: &'static str,
    /// The one attribute the reader looks at, so the request asks for nothing
    /// else (the tree layer carries fifteen). Empty for a source that reads none.
    pub attribute: &'static str,
    pub yields: Yields,
    /// The national-register content this source stands in for. Declared rather
    /// than derived: a reader is a function and cannot be asked what it might
    /// return, and "this source is the authority on lamp posts here" is a claim
    /// about the data, not the code.
    pub supplies: &'static [Supply],
}

/// One municipality's register.
pub struct Register {
    pub id: &'static str,
    pub attribution: &'static str,
    /// The register's reach in RD metres, `[xmin, ymin, xmax, ymax]` — the tree
    /// layer's published extent, so the city's reach and not a guess at it.
    pub bounds_rd: [f64; 4],
    pub wfs: &'static str,
    pub sources: &'static [Source],
}

/// Every city whose register atlas reads.
pub static REGISTERS: &[Register] = &[GRONINGEN];

// ─── Groningen ───────────────────────────────────────────────────────────────

const GRONINGEN: Register = Register {
    id: "groningen",
    attribution: "Street objects: gemeente Groningen open data (CC BY 4.0)",
    bounds_rd: [227_074.0, 569_831.0, 246_669.0, 592_673.0],
    wfs: "https://maps.groningen.nl/geoserver/wfs",
    sources: &[
        Source {
            id: "trees",
            layer: "geo-data:Bomen gemeente Groningen",
            geometry: "GEOM",
            attribute: "BOOMHOOGTE",
            yields: Yields::Trees {
                height: |f| f.str("BOOMHOOGTE").and_then(height_band_m),
            },
            supplies: &[Supply::Trees],
        },
        Source {
            id: "lamps",
            layer: "geo-data:Lichtmasten gemeente Groningen",
            geometry: "GEOM",
            attribute: "LICHTPUNT_SOORT",
            yields: Yields::Props {
                // Over one cell: 481 `Lichtmast`, 8 `Grondspot`, 3
                // `Tunnelverlichting`, 1 `Klok`. Only the column has a form here.
                kind: |f| match f.str("LICHTPUNT_SOORT") {
                    Some("Lichtmast") => Some((FurnitureKind::Lamp, None)),
                    _ => None,
                },
            },
            supplies: &[Supply::Prop(FurnitureKind::Lamp)],
        },
        Source {
            id: "containers",
            layer: "geo-data:CONTAINERS",
            geometry: "GEOM",
            attribute: "FRACTIE",
            yields: Yields::Props {
                // The fraction is a colour, not a form: a bottle bank and a paper
                // bank are the same six boxes.
                kind: |f| {
                    Some((
                        FurnitureKind::Recycling,
                        Some(fraction_colour(f.str("FRACTIE")?)),
                    ))
                },
            },
            supplies: &[Supply::Prop(FurnitureKind::Recycling)],
        },
        Source {
            id: "fountains",
            layer: "geo-data:Fonteinen_gemeente_Groningen",
            geometry: "GEOM",
            attribute: "SOORT_KUNSTWERK",
            yields: Yields::Props {
                // `Fontein` on every feature seen, but matched rather than
                // accepted blanket: the collection is the city's *kunstwerken*
                // register, and another `SOORT` in it later is a thing with no
                // form here, not a fountain.
                kind: |f| match f.str("SOORT_KUNSTWERK") {
                    Some("Fontein") => Some((FurnitureKind::Fountain, None)),
                    _ => None,
                },
            },
            supplies: &[Supply::Prop(FurnitureKind::Fountain)],
        },
        Source {
            id: "chargers",
            layer: "geo-data:Laadpalen_gemeente_Groningen",
            geometry: "GEOM",
            attribute: "STATUS_LAADPAAL",
            yields: Yields::Props {
                // A charge point is a box on a plinth. One status value exists
                // today; anything else (planned, withdrawn) is refused, because a
                // missing charge point is a smaller wrong than an invented one.
                kind: |f| match f.str("STATUS_LAADPAAL") {
                    Some("Bestaande laadpaal") => Some((FurnitureKind::Cabinet, None)),
                    _ => None,
                },
            },
            supplies: &[Supply::Prop(FurnitureKind::Cabinet)],
        },
        Source {
            id: "shelters",
            // The lighting register's schema: every object an `Abri`, the shelter
            // itself, with the lamp number it is wired to.
            layer: "geo-data:Bushaltes gemeente Groningen",
            geometry: "GEOM",
            attribute: "LICHTPUNT_SOORT",
            yields: Yields::Props {
                kind: |f| match f.str("LICHTPUNT_SOORT") {
                    Some("Abri") => Some((FurnitureKind::Shelter, None)),
                    _ => None,
                },
            },
            supplies: &[Supply::Prop(FurnitureKind::Shelter)],
        },
        Source {
            id: "signals",
            // The lighting schema again: 1,097 `Verkeerslicht`, 393 `VRI
            // Voetgangerslicht` and 557 `Schakelkast`, the controller at the kerb.
            layer: "geo-data:Verkeerslichten gemeente Groningen",
            geometry: "GEOM",
            attribute: "LICHTPUNT_SOORT",
            yields: Yields::Props {
                kind: |f| match f.str("LICHTPUNT_SOORT") {
                    Some("Verkeerslicht" | "VRI Voetgangerslicht") => {
                        Some((FurnitureKind::TrafficLight, None))
                    }
                    Some("Schakelkast") => Some((FurnitureKind::Cabinet, None)),
                    _ => None,
                },
            },
            // Not `Cabinet`: the chargers already speak for the city's cabinets.
            supplies: &[Supply::Prop(FurnitureKind::TrafficLight)],
        },
        Source {
            id: "ticket machines",
            // 357 over the city, against OSM's 68. A pay-and-display column is a
            // box on a post, which is the form `vending_machine` already gets.
            // Supplies nothing new: the chargers already speak for cabinets.
            layer: "geo_info:Parkeerautomaten gemeente Groningen",
            geometry: "GEOM",
            attribute: "",
            yields: Yields::Props {
                kind: |_| Some((FurnitureKind::Cabinet, None)),
            },
            supplies: &[],
        },
        Source {
            id: "parking bays",
            // 34,994 over the city, each one bay — measured 2026-10-19 over a
            // square kilometre of the centre: parallel bays a median 5.7 × 2.0 m,
            // perpendicular 4.9 × 2.3, herringbone 6.8 × 2.2. OSM has 1,741.
            layer: "geo_info:Parkeervakken gemeente Groningen",
            geometry: "geom",
            attribute: "",
            yields: Yields::Bays,
            supplies: &[],
        },
    ],
};

/// What a Dutch waste fraction is painted, in sRGB: the colours the lids are
/// here. `RESTAFVAL`, by far the commonest, keeps the painted steel every
/// container was drawn in before, and so does a fraction nobody named.
fn fraction_colour(fraction: &str) -> u32 {
    match fraction {
        "PAPIER" => 0x3E_5E_8C,
        "GLAS" => 0x3F_6B_4A,
        "TEXTIEL" => 0xB5_6A_36,
        _ => 0x9A_98_93,
    }
}

/// A Dutch height band — `"15 tot 18 m."`, `"tot 6 m."`, `"24 m. en hoger"` — as
/// one number in metres.
///
/// Read off the numbers rather than matched against the eight spellings seen
/// today: the register is a city's operational database and a ninth band costs
/// nothing to add there. The open bands are the one judgement: the lowest at
/// three quarters of its ceiling (a tree at the bottom of it is a sapling the
/// register does not carry), the highest a little above its floor.
#[must_use]
pub fn height_band_m(band: &str) -> Option<f32> {
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

/// RD New's extent over the Netherlands. A coordinate outside it is another
/// projection — Web Mercator is what the GeoServer sends when nobody asks — and
/// converting it as RD plants the object in another country.
fn is_rd(x: f64, y: f64) -> bool {
    (0.0..300_000.0).contains(&x) && (289_000.0..629_000.0).contains(&y)
}

/// One layer over `bbox`, as GeoJSON in RD metres.
///
/// **`srsName` is what makes it RD**; the server's default is Web Mercator, which
/// once put every tree the client drew from this city outside its cell.
/// `propertyName` asks for the geometry and the one attribute and nothing else.
/// There is no paging: the server answers a whole city in one response, and
/// pages would need a `sortBy` on a column each layer names differently (it
/// refuses `startIndex` without one). A response that comes back short of its
/// own `numberMatched` fails the run rather than writing a thinned city.
fn source_url(register: &Register, source: &Source, bbox: [f64; 4]) -> String {
    let properties = if source.attribute.is_empty() {
        source.geometry.to_string()
    } else {
        format!("{},{}", source.geometry, source.attribute)
    };
    let [x0, y0, x1, y1] = bbox;
    format!(
        "{}?{}",
        register.wfs,
        query(&[
            ("service", "WFS"),
            ("version", "2.0.0"),
            ("request", "GetFeature"),
            ("typeNames", source.layer),
            ("outputFormat", "application/json"),
            ("srsName", "EPSG:28992"),
            ("propertyName", &properties),
            (
                "bbox",
                &format!("{x0:.0},{y0:.0},{x1:.0},{y1:.0},EPSG:28992")
            ),
        ])
    )
}

/// What one source contributed, in lat/lng.
#[derive(Debug, Default, PartialEq)]
pub struct Read {
    pub trees: Vec<Tree>,
    pub props: Vec<Prop>,
    pub bays: Vec<Vec<(f64, f64)>>,
    /// Features refused by the reader or outside RD.
    pub refused: usize,
}

/// One layer's answer, read.
///
/// # Errors
///
/// When the page is not a feature collection, came back truncated, or carried
/// nothing in RD at all — so a wrong answer is never written as the city's.
pub fn read_source(doc: &Value, yields: &Yields) -> Result<Read, String> {
    let features = doc["features"]
        .as_array()
        .ok_or_else(|| "no features array".to_string())?;
    let matched = doc["numberMatched"].as_u64();
    if let Some(matched) = matched {
        if matched > features.len() as u64 {
            return Err(format!(
                "truncated: {} of {matched} features returned",
                features.len()
            ));
        }
    }
    // Exact repeats are ghost versions of one object; two identical meshes in one
    // place z-fight their own surfaces.
    let mut seen = HashSet::new();
    let mut out = Read::default();
    let mut not_rd = 0usize;
    for f in features {
        let fields = Fields(&f["properties"]);
        let geometry = &f["geometry"];
        if let Yields::Bays = yields {
            for poly in polygon_rings(geometry) {
                let Some(outer) = poly.first().filter(|r| r.len() >= 4) else {
                    continue;
                };
                if !outer.iter().all(|&[x, y]| is_rd(x, y)) {
                    not_rd += 1;
                    continue;
                }
                let key: Vec<(u64, u64)> = outer
                    .iter()
                    .map(|[x, y]| (x.to_bits(), y.to_bits()))
                    .collect();
                if !seen.insert(key) {
                    continue;
                }
                out.bays.push(
                    outer
                        .iter()
                        .map(|&[x, y]| {
                            let (lat, lng) = rd_to_lat_lng(x, y);
                            (round(lat, 7), round(lng, 7))
                        })
                        .collect(),
                );
            }
            continue;
        }
        let coords = &geometry["coordinates"];
        let (Some(x), Some(y)) = (coords[0].as_f64(), coords[1].as_f64()) else {
            out.refused += 1;
            continue;
        };
        if !is_rd(x, y) {
            not_rd += 1;
            continue;
        }
        if !seen.insert(vec![(x.to_bits(), y.to_bits())]) {
            continue;
        }
        let (lat, lng) = rd_to_lat_lng(x, y);
        let (lat, lng) = (round(lat, 7), round(lng, 7));
        match yields {
            Yields::Trees { height } => out.trees.push(Tree(
                lat,
                lng,
                height(&fields).map(|h| (h * 10.0).round() / 10.0),
            )),
            Yields::Props { kind } => match kind(&fields) {
                Some((kind, paint)) => out.props.push(Prop(lat, lng, kind.slug().into(), paint)),
                None => out.refused += 1,
            },
            Yields::Bays => unreachable!("handled above"),
        }
    }
    out.refused += not_rd;
    if not_rd > 0 && out.trees.is_empty() && out.props.is_empty() && out.bays.is_empty() {
        return Err(format!(
            "{not_rd} features outside the RD grid: not RD coordinates"
        ));
    }
    Ok(out)
}

// ─── Wikidata's public art ───────────────────────────────────────────────────

const WIKIDATA_SPARQL: &str = "https://query.wikidata.org/sparql";
const WIKIDATA_ATTRIBUTION: &str = "Public art: Wikidata (CC0)";

/// The classes taken: things that stand in a square by themselves. Wikidata's
/// broader `sculpture`, `work of art`, `memorial` and `monument` are **not**
/// taken — over Groningen half of what it files under those is on a façade, in a
/// bridge or set in the paving (a sample of 40 checked by hand, 2026-10-19), and
/// nothing on Wikidata tells the two apart. Measured over the municipality: 64
/// items of these classes, against 562 of the broad ones.
const FREE_STANDING_CLASSES: &[&str] = &[
    "Q179700",   // statue
    "Q241045",   // bust
    "Q2293362",  // group of sculptures
    "Q575759",   // war memorial
    "Q11734477", // memorial stone
];

/// What makes one *not* a plinth in a square even so, matched against an item's
/// classes **and its genres** (`P136`): a war memorial that is a plaque on a
/// church is a plaque, and "Wandsculpturen met torsende mannen", two figures on a
/// façade, is a `sculpture` whose genre is *architectural sculpture*. Fountains
/// are the city's own register's (`fountains` above).
const NOT_FREE_STANDING: &[&str] = &[
    "Q245117",    // relief sculpture
    "Q721747",    // commemorative plaque
    "Q2849607",   // gable stone
    "Q219423",    // mural
    "Q2605386",   // tile panel
    "Q133067",    // mosaic
    "Q116452632", // memorial wall
    "Q26703203",  // stolperstein
    "Q56055312",  // sepulchral monument
    "Q483453",    // fountain
    "Q3476515",   // architectural sculpture (genre)
    "Q1421557",   // light art (genre)
    "Q378604",    // new media art (genre)
    "Q326478",    // land art (genre)
];

/// Every free-standing work Wikidata places inside `[west, south, east, north]`,
/// with all its classes and genres, leaving out the demolished and removed
/// (`P576`). One request for the whole area.
#[must_use]
pub fn art_query(bbox: [f64; 4]) -> String {
    let [west, south, east, north] = bbox;
    let classes: Vec<String> = FREE_STANDING_CLASSES
        .iter()
        .map(|q| format!("wd:{q}"))
        .collect();
    format!(
        "SELECT ?item ?coord (GROUP_CONCAT(DISTINCT STRAFTER(STR(?type), \"/entity/\"); separator=\" \") AS ?types) WHERE {{\
         SERVICE wikibase:box {{ ?item wdt:P625 ?coord . \
         bd:serviceParam wikibase:cornerSouthWest \"Point({west:.5} {south:.5})\"^^geo:wktLiteral . \
         bd:serviceParam wikibase:cornerNorthEast \"Point({east:.5} {north:.5})\"^^geo:wktLiteral . }} \
         VALUES ?art {{ {} }} ?item wdt:P31 ?art . \
         {{ ?item wdt:P31 ?type . }} UNION {{ ?item wdt:P136 ?type . }} \
         FILTER NOT EXISTS {{ ?item wdt:P576 ?gone . }} }} GROUP BY ?item ?coord",
        classes.join(" ")
    )
}

/// A free-standing work as Wikidata places it.
#[derive(Clone, Debug, PartialEq)]
pub struct Artwork {
    /// The Q-id, which an OSM object names in its `wikidata` tag.
    pub qid: String,
    pub lat: f64,
    pub lng: f64,
}

/// The works in a SPARQL answer, each item once, less those whose class or genre
/// says they are not free-standing (counted).
///
/// # Errors
///
/// When the answer is not a SPARQL result set.
pub fn read_art(doc: &Value) -> Result<(Vec<Artwork>, usize), String> {
    let rows = doc["results"]["bindings"]
        .as_array()
        .ok_or_else(|| "not a SPARQL result set".to_string())?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut refused = 0usize;
    for row in rows {
        let qid = row["item"]["value"]
            .as_str()
            .unwrap_or_default()
            .rsplit('/')
            .next()
            .unwrap_or_default();
        let types = row["types"]["value"].as_str().unwrap_or_default();
        if types.split(' ').any(|t| NOT_FREE_STANDING.contains(&t)) {
            refused += 1;
            continue;
        }
        // `Point(lng lat)`. An item with two coordinates is two rows; the first
        // stands, because one statue is not two.
        let Some(point) = row["coord"]["value"]
            .as_str()
            .and_then(|p| p.strip_prefix("Point("))
            .and_then(|p| p.strip_suffix(')'))
        else {
            continue;
        };
        let mut it = point.split(' ').filter_map(|s| s.parse::<f64>().ok());
        let (Some(lng), Some(lat)) = (it.next(), it.next()) else {
            continue;
        };
        if seen.insert(qid.to_string()) {
            out.push(Artwork {
                qid: qid.to_string(),
                lat: round(lat, 7),
                lng: round(lng, 7),
            });
        }
    }
    Ok((out, refused))
}

// ─── What OpenStreetMap already draws ────────────────────────────────────────

/// How close an OSM artwork must stand to a Wikidata one, neither naming the
/// other, for the two to be one statue.
///
/// The one tolerance in this module, and it sits in a gap, not on a slope:
/// measured over Groningen (2026-10-19) the unlinked Wikidata works nearest an
/// OSM artwork are 0–21 m from it, and the next is 36 m — two registers placing
/// one statue, then separate statues. Wikidata's coordinates are often from a
/// list compiled by hand, so a few metres either way is ordinary.
const SAME_WORK_M: f64 = 25.0;

/// The map tile draws OSM's `tourism=artwork` already, so a Wikidata work OSM has
/// is left to it and only the ones it lacks are added. Asked of **our own**
/// Overpass (`tall::overpass_url`), once for the area.
#[must_use]
pub fn osm_art_query(bbox: [f64; 4]) -> String {
    let [west, south, east, north] = bbox;
    format!(
        "[out:json][timeout:60];nwr[\"tourism\"=\"artwork\"]({south:.5},{west:.5},{north:.5},{east:.5});out center tags;"
    )
}

/// OSM's artworks: where each stands, and the Wikidata item it names, if any.
///
/// # Errors
///
/// When the answer is not an Overpass result.
pub fn read_osm_art(doc: &Value) -> Result<Vec<((f64, f64), Option<String>)>, String> {
    let elements = doc["elements"]
        .as_array()
        .ok_or_else(|| "not an Overpass result".to_string())?;
    Ok(elements
        .iter()
        .filter_map(|e| {
            let at = |o: &Value| Some((o["lat"].as_f64()?, o["lon"].as_f64()?));
            let pos = at(e).or_else(|| at(&e["center"]))?;
            let qid = e["tags"]["wikidata"].as_str().map(str::to_string);
            Some((pos, qid))
        })
        .collect())
}

/// Whether OSM already has this work: it names it, or stands within
/// [`SAME_WORK_M`] of it.
#[must_use]
pub fn osm_has(work: &Artwork, osm: &[((f64, f64), Option<String>)]) -> bool {
    osm.iter().any(|((lat, lng), qid)| {
        qid.as_deref() == Some(work.qid.as_str()) || {
            let dy = (lat - work.lat) * 111_320.0;
            let dx = (lng - work.lng) * 111_320.0 * work.lat.to_radians().cos();
            dx.hypot(dy) < SAME_WORK_M
        }
    })
}

// ─── The run ─────────────────────────────────────────────────────────────────

/// A cell's centre in RD.
fn cell_centre_rd((x, y): Cell) -> [f64; 2] {
    let (north, west) = tile_to_lat_lng(x, y, CACHE_ZOOM);
    let (south, east) = tile_to_lat_lng(x + 1, y + 1, CACHE_ZOOM);
    wgs84_to_rd((north + south) / 2.0, (west + east) / 2.0)
}

fn inside([x, y]: [f64; 2], [x0, y0, x1, y1]: [f64; 4]) -> bool {
    (x0..x1).contains(&x) && (y0..y1).contains(&y)
}

/// Every cell whose centre lies inside an RD box.
#[must_use]
pub fn cells_centred_in(bbox: [f64; 4]) -> BTreeSet<Cell> {
    let [west, south, east, north] = wgs84_bbox(bbox);
    let (x0, y0) = cartopolis_core::lat_lng_to_tile(north, west, CACHE_ZOOM);
    let (x1, y1) = cartopolis_core::lat_lng_to_tile(south, east, CACHE_ZOOM);
    (y0..=y1)
        .flat_map(|y| (x0..=x1).map(move |x| (x, y)))
        .filter(|c| inside(cell_centre_rd(*c), bbox))
        .collect()
}

/// The RD envelope of a set of cells, from each one's corners **and** edge
/// midpoints: RD's grid is rotated against Web Mercator's, so a box over the
/// corners alone can miss ground a cell covers.
fn rd_envelope(cells: &BTreeSet<Cell>) -> [f64; 4] {
    let mut env = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for &(x, y) in cells {
        let (north, west) = tile_to_lat_lng(x, y, CACHE_ZOOM);
        let (south, east) = tile_to_lat_lng(x + 1, y + 1, CACHE_ZOOM);
        for lat in [south, (south + north) / 2.0, north] {
            for lng in [west, (west + east) / 2.0, east] {
                let [rx, ry] = wgs84_to_rd(lat, lng);
                env = [
                    env[0].min(rx),
                    env[1].min(ry),
                    env[2].max(rx),
                    env[3].max(ry),
                ];
            }
        }
    }
    env
}

fn intersect(a: [f64; 4], b: [f64; 4]) -> Option<[f64; 4]> {
    let r = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    (r[0] < r[2] && r[1] < r[3]).then_some(r)
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let area = cells_centred_in(ctx.bbox);
    let mut docs: BTreeMap<Cell, CellDoc> = area.iter().map(|c| (*c, CellDoc::default())).collect();
    let mut counts: Vec<Count> = Vec::new();
    let mut attributions: Vec<&str> = Vec::new();

    for register in REGISTERS {
        let Some(reach) = intersect(ctx.bbox, register.bounds_rd) else {
            continue;
        };
        let covered: BTreeSet<Cell> = area
            .iter()
            .copied()
            .filter(|c| inside(cell_centre_rd(*c), reach))
            .collect();
        if covered.is_empty() {
            continue;
        }
        ctx.log(format!("{}: {} cellen", register.id, covered.len()));
        attributions.push(register.attribution);
        let fetch_box = rd_envelope(&covered);
        for source in register.sources {
            ctx.check()?;
            let url = source_url(register, source, fetch_box);
            let doc = get_json(ctx, &url, source.id)?;
            let read = read_source(&doc, &source.yields).map_err(|e| {
                PipelineError::Failed(format!("{}/{}: {e}", register.id, source.id))
            })?;
            let mut kept = 0usize;
            for t in read.trees {
                let cell = lat_lng_to_cell(t.0, t.1);
                if let Some(d) = docs.get_mut(&cell).filter(|_| covered.contains(&cell)) {
                    d.trees.push(t);
                    kept += 1;
                }
            }
            for p in read.props {
                let cell = lat_lng_to_cell(p.0, p.1);
                if let Some(d) = docs.get_mut(&cell).filter(|_| covered.contains(&cell)) {
                    d.props.push(p);
                    kept += 1;
                }
            }
            for bay in read.bays {
                // Binned by the bay's first corner; a bay is two metres across,
                // and the cell that holds that corner draws it whole.
                let cell = lat_lng_to_cell(bay[0].0, bay[0].1);
                if let Some(d) = docs.get_mut(&cell).filter(|_| covered.contains(&cell)) {
                    d.bays.push(bay);
                    kept += 1;
                }
            }
            ctx.log(format!(
                "{}/{}: {kept} in the area, {} refused",
                register.id, source.id, read.refused
            ));
            counts.push(Count::new(source.id, kept));
            for cell in &covered {
                if let Some(d) = docs.get_mut(cell) {
                    for supply in source.supplies {
                        d.supplies.push(supply.token().to_string());
                    }
                }
            }
        }
    }

    // Wikidata is national, so it covers the whole area — but it **adds** to the
    // tile's `tourism=artwork` rather than replacing it: only the free-standing
    // classes are taken (64 over Groningen, against OSM's 173 artworks), so OSM
    // stays the main source and Wikidata fills in the statues it lacks.
    ctx.log("wikidata: beelden en monumenten…");
    let area = wgs84_bbox(ctx.bbox);
    let url = format!(
        "{WIKIDATA_SPARQL}?{}",
        query(&[("query", &art_query(area)), ("format", "json")])
    );
    let doc = get_json(ctx, &url, "wikidata")?;
    let (art, refused) =
        read_art(&doc).map_err(|e| PipelineError::Failed(format!("wikidata: {e}")))?;
    let url = format!(
        "{}?{}",
        super::tall::overpass_url(),
        query(&[("data", &osm_art_query(area))])
    );
    let doc = get_json(ctx, &url, "overpass")?;
    let osm = read_osm_art(&doc).map_err(|e| PipelineError::Failed(format!("overpass: {e}")))?;
    let (mut n_art, mut in_osm) = (0usize, 0usize);
    for work in &art {
        let Some(d) = docs.get_mut(&lat_lng_to_cell(work.lat, work.lng)) else {
            continue;
        };
        if osm_has(work, &osm) {
            in_osm += 1;
            continue;
        }
        d.props.push(Prop(
            work.lat,
            work.lng,
            FurnitureKind::Artwork.slug().into(),
            None,
        ));
        n_art += 1;
    }
    ctx.log(format!(
        "wikidata: {n_art} statues and memorials added, {in_osm} already in OSM, \
         {refused} on a wall or in the paving"
    ));
    counts.push(Count::new("artworks", n_art));
    attributions.push(WIKIDATA_ATTRIBUTION);

    for (cell, doc) in &docs {
        ctx.check()?;
        ctx.write_doc(&cell_path("registers", *cell), doc)?;
    }
    ctx.write_coverage(
        "registers/coverage.json",
        &Manifest {
            // The run: the client keys its cache of these cells on it, so a
            // re-run (a felled tree, a new statue) is never read from an old copy.
            stamp: Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs()),
            ),
            ..Manifest::new(CACHE_ZOOM, sorted_cells(docs.keys().copied())).described(
                ctx.name.as_str(),
                attributions.join(" · "),
                "Gemeente Groningen GeoServer (WFS); Wikidata SPARQL; our Overpass",
                ctx.bbox,
            )
        },
    )?;
    ctx.log(format!("done: {} cells", docs.len()));
    Ok(DatasetResult {
        cells: docs.len(),
        counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source(id: &str) -> &'static Source {
        GRONINGEN.sources.iter().find(|s| s.id == id).unwrap()
    }

    #[test]
    fn height_bands_read_off_their_numbers() {
        assert_eq!(height_band_m("15 tot 18 m."), Some(16.5));
        assert_eq!(height_band_m("tot 6 m."), Some(4.5));
        assert_eq!(height_band_m("24 m. en hoger"), Some(24.0 * 1.15));
        assert_eq!(height_band_m("onbekend"), None);
    }

    /// The poplar on the study quay, and a page the server sent in Web Mercator
    /// because nobody asked for RD — refused, never written.
    #[test]
    fn coordinates_are_asked_for_in_rd_and_refused_otherwise() {
        let url = source_url(&GRONINGEN, source("trees"), [1.0, 2.0, 3.0, 4.0]);
        assert!(url.contains("srsName=EPSG%3A28992"), "{url}");
        let page = |x: f64, y: f64| {
            json!({"features":[{"geometry":{"type":"Point","coordinates":[x, y]},
                                "properties":{"BOOMHOOGTE":"15 tot 18 m."}}]})
        };
        assert!(read_source(&page(732_717.02, 7_024_297.87), &source("trees").yields).is_err());
        let read = read_source(&page(234_806.13, 582_449.68), &source("trees").yields).unwrap();
        let Tree(lat, lng, h) = read.trees[0];
        assert!(
            (lat - 53.2227).abs() < 1e-3 && (lng - 6.5821).abs() < 1e-3,
            "{lat} {lng}"
        );
        assert_eq!(h, Some(16.5));
    }

    #[test]
    fn a_truncated_answer_fails_rather_than_thinning_the_city() {
        let doc = json!({"numberMatched": 2, "features":[
            {"geometry":{"type":"Point","coordinates":[234_806.0, 582_449.0]},"properties":{}}]});
        assert!(read_source(&doc, &source("trees").yields).is_err());
    }

    #[test]
    fn readers_refuse_what_has_no_form() {
        let doc = json!({"features":[
            {"geometry":{"type":"Point","coordinates":[234_800.0, 582_400.0]},
             "properties":{"LICHTPUNT_SOORT":"Lichtmast"}},
            {"geometry":{"type":"Point","coordinates":[234_810.0, 582_400.0]},
             "properties":{"LICHTPUNT_SOORT":"Grondspot"}},
            {"geometry":{"type":"Point","coordinates":[234_800.0, 582_400.0]},
             "properties":{"LICHTPUNT_SOORT":"Lichtmast"}}]});
        let read = read_source(&doc, &source("lamps").yields).unwrap();
        // One column; the ground spot refused; the exact repeat a ghost.
        assert_eq!(read.props.len(), 1);
        assert_eq!(read.props[0].2, "lamp");
        assert_eq!(read.refused, 1);
    }

    #[test]
    fn a_container_is_painted_by_its_fraction() {
        let doc = json!({"features":[
            {"geometry":{"type":"Point","coordinates":[234_800.0, 582_400.0]},
             "properties":{"FRACTIE":"GLAS"}},
            {"geometry":{"type":"Point","coordinates":[234_900.0, 582_400.0]},
             "properties":{"FRACTIE":"RESTAFVAL"}}]});
        let read = read_source(&doc, &source("containers").yields).unwrap();
        assert_eq!(read.props[0].3, Some(0x3F_6B_4A));
        assert_eq!(read.props[1].3, Some(0x9A_98_93));
    }

    #[test]
    fn a_bay_is_its_outer_ring_in_lat_lng() {
        let doc = json!({"features":[{"geometry":{"type":"Polygon","coordinates":[[
            [234_800.0, 582_400.0],[234_805.7, 582_400.0],[234_805.7, 582_402.0],
            [234_800.0, 582_402.0],[234_800.0, 582_400.0]]]},"properties":{}}]});
        let read = read_source(&doc, &GRONINGEN.sources.last().unwrap().yields).unwrap();
        assert_eq!(read.bays.len(), 1);
        assert_eq!(read.bays[0].len(), 5);
        assert!((read.bays[0][0].0 - 53.22).abs() < 0.01);
    }

    #[test]
    fn art_on_a_wall_is_not_stood_in_the_street() {
        let doc = json!({"results":{"bindings":[
            {"item":{"value":"http://www.wikidata.org/entity/Q1"},
             "coord":{"value":"Point(6.56732574 53.21844151)"},
             "types":{"value":"Q179700"}},
            {"item":{"value":"http://www.wikidata.org/entity/Q2"},
             "coord":{"value":"Point(6.5 53.2)"},
             "types":{"value":"Q575759 Q721747"}},
            {"item":{"value":"http://www.wikidata.org/entity/Q1"},
             "coord":{"value":"Point(6.6 53.3)"},
             "types":{"value":"Q179700"}}]}});
        let (art, refused) = read_art(&doc).unwrap();
        assert_eq!(
            art,
            vec![Artwork {
                qid: "Q1".into(),
                lat: 53.218_441_5,
                lng: 6.567_325_7
            }]
        );
        assert_eq!(refused, 1, "the war memorial that is a plaque");
    }

    /// A work OSM names, or stands beside, is OSM's to draw; one it lacks is ours.
    #[test]
    fn a_statue_osm_already_has_is_left_to_it() {
        let doc = json!({"elements":[
            {"type":"node","lat":53.2,"lon":6.5,"tags":{"tourism":"artwork","wikidata":"Q7"}},
            {"type":"way","center":{"lat":53.21,"lon":6.51},"tags":{"tourism":"artwork"}}]});
        let osm = read_osm_art(&doc).unwrap();
        assert_eq!(osm.len(), 2);
        let work = |qid: &str, lat: f64, lng: f64| Artwork {
            qid: qid.into(),
            lat,
            lng,
        };
        assert!(osm_has(&work("Q7", 53.3, 6.6), &osm), "named, however far");
        assert!(osm_has(&work("Q8", 53.2101, 6.5101), &osm), "13 m away");
        assert!(!osm_has(&work("Q9", 53.2105, 6.5105), &osm), "60 m away");
    }

    #[test]
    fn the_art_query_asks_wikidata_s_box_in_lng_lat() {
        let q = art_query([6.45, 53.1, 6.77, 53.32]);
        assert!(q.contains("Point(6.45000 53.10000)"), "{q}");
        assert!(q.contains("Point(6.77000 53.32000)"), "{q}");
        assert!(q.contains("wd:Q179700"));
        assert!(!q.contains("wd:Q860861"), "not the broad `sculpture`");
    }

    /// Every cell whose centre is in the area, and none whose centre is not —
    /// the rule that makes two adjoining runs share no cell.
    #[test]
    fn cells_are_the_ones_centred_in_the_area() {
        let bbox = [233_000.0, 581_000.0, 236_000.0, 584_000.0];
        let cells = cells_centred_in(bbox);
        assert!(!cells.is_empty());
        for c in &cells {
            assert!(inside(cell_centre_rd(*c), bbox));
        }
        assert!(cells.contains(&lat_lng_to_cell(53.2227, 6.5821)));
    }

    #[test]
    fn supply_tokens_are_slugs_the_client_can_read() {
        for r in REGISTERS {
            for s in r.sources {
                for supply in s.supplies {
                    let t = supply.token();
                    assert!(
                        t == SUPPLIES_TREES || FurnitureKind::from_slug(t).is_some(),
                        "{t}"
                    );
                }
            }
        }
    }
}
