//! What is planned where in Groningen, mirrored: the municipality's official
//! notices with the places they carry, the council business that names the
//! same places, served to every client from here at `/plans/groningen.json`
//! (the client's `systems::plans` draws it; `cartopolis_core::plans` is the
//! shape, `docs/notes/plans.md` the sources and what was measured on them).
//!
//! Three open sources, none of them asking for a key:
//!
//! - **The notices**: KOOP's repository of official publications, the
//!   Gemeenteblad's records for the last [`NOTICE_DAYS`] days. 96 % carry a
//!   point, line or area in WGS84 (`overheidwetgeving:locatiegebied`) and most a
//!   response deadline (`datumEindeReactietermijn`).
//! - **The council**: the council site's own JSON (GemeenteOplossingen), every
//!   meeting from [`COUNCIL_DAYS_BACK`] days ago to [`COUNCIL_DAYS_AHEAD`]
//!   ahead, with its agenda items and their documents, in one answer.
//! - **The names**: PDOK's location server, every street, neighbourhood and
//!   district in the municipality with its centroid — the gazetteer a council
//!   item's title is matched against, because council items carry no place.
//!
//! **Why a mirror rather than each client asking.** The notices take six pages
//! of XML a year and the matching needs all three sources at once; done here it
//! is one ~1 MB JSON a client reads in one request, and the layer keeps working
//! while any source is down. The last good copy is kept on disk and served
//! until a newer one is built; a build with too few notices never replaces it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use cartopolis_core::plans::{
    council_key, short_title, CouncilDocument, CouncilItem, CouncilPlace, CouncilStep, Notice,
    NoticeKind, PlansFeed,
};
use serde::Deserialize;

/// The municipality as KOOP names it in `dt.creator`.
pub const MUNICIPALITY: &str = "Groningen";
/// Its CBS code, as PDOK's location server filters by it.
const MUNICIPALITY_CODE: &str = "0014";
/// The council site.
const COUNCIL_SITE: &str = "https://gemeenteraad.groningen.nl";
/// KOOP's search endpoint.
const SRU: &str = "https://repository.overheid.nl/sru";
/// PDOK's location server.
const LOCATIESERVER: &str = "https://api.pdok.nl/bzk/locatieserver/search/v3_1/free";

/// How far back notices are kept: the layer's longest "Show" setting.
pub const NOTICE_DAYS: u64 = 365;
/// How far back council meetings are read.
const COUNCIL_DAYS_BACK: u64 = 730;
/// How far ahead: agendas are published weeks before the meeting.
const COUNCIL_DAYS_AHEAD: u64 = 120;
/// SRU records per page; KOOP's ceiling.
const SRU_PAGE: usize = 1000;
/// PDOK rows per page; its ceiling.
const PDOK_PAGE: usize = 100;

/// How often the feed is rebuilt. Notices are published on working days, a
/// few dozen a day.
pub const REFRESH: Duration = Duration::from_secs(3 * 3600);
/// How long to wait after a failed build.
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(1800);
/// How old the gazetteer may be before it is fetched again: street names
/// change a few times a year.
const GAZETTEER_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// Fewer notices than this in a year is not the Gemeenteblad — Groningen
/// published 1,336 in the three months to 2026-10-29 — and is not allowed to
/// replace a good feed.
pub const MIN_NOTICES: usize = 200;
/// A notice's area is kept to this many vertices.
const MAX_AREA_POINTS: usize = 64;
/// Council items listed on one notice.
const MAX_COUNCIL_PER_NOTICE: usize = 3;

/// Council bodies whose agendas are about the business itself. The agenda
/// committee, the presidium and the clerk's meetings list the same items again
/// as procedure, and would put a meeting nobody discussed anything at on every
/// item's line.
const COUNCIL_BODIES: &[&str] = &[
    "gemeenteraad",
    "Politieke woensdag (Dag agenda)",
    "Politiek Vragenuur",
    "Extra bijeenkomsten",
    "Actualiteitendebat",
];

/// Names in the gazetteer that are also ordinary words in a council title, so
/// matching on them places items where they are not about. "Groningen" first:
/// it is a neighbourhood's name too, and it stands in nearly every notice's
/// address and in every "gemeente Groningen" title — left in, the first live
/// build (2026-10-30) linked 4,357 of 5,520 notices to by-laws.
const NOT_PLACES: &[&str] = &[
    "Groningen",
    "Centrum",
    "Haven",
    "Markt",
    "Plein",
    "Kade",
    "Dijk",
    "Park",
    "Molen",
    "School",
    "Kerk",
    "Station",
    "Zuid",
    "Noord",
    "Oost",
    "West",
    "Buiten",
    "Hoven",
    "Esch",
    "Kamp",
    "Ruim",
    "Drents",
];

/// How long a client or a cache may keep an answer before asking again.
const CLIENT_MAX_AGE_S: u64 = 1800;

/// One name in the gazetteer and where it is.
#[derive(Debug, Clone, PartialEq, serde::Serialize, Deserialize)]
pub struct PlaceName {
    pub name: String,
    pub lat: f64,
    pub lng: f64,
}

/// A notice as read from KOOP, before it is classified and linked.
#[derive(Debug, Clone, PartialEq)]
pub struct RawNotice {
    pub id: String,
    pub koop_type: String,
    pub title: String,
    pub date: String,
    pub respond_until: Option<String>,
    pub place: String,
    pub lat: f64,
    pub lng: f64,
    pub area: Vec<[f64; 2]>,
}

/// A meeting as the council site answers it.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct Meeting {
    pub id: u64,
    pub dmu: Dmu,
    #[serde(rename = "fullUrl", default)]
    pub full_url: String,
    #[serde(default)]
    pub confidential: bool,
    /// `YYYY-MM-DD HH:MM:SS`.
    pub date: String,
    #[serde(default)]
    pub items: Vec<MeetingItem>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct Dmu {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct MeetingItem {
    pub title: String,
    #[serde(default)]
    pub documents: Vec<MeetingDocument>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct MeetingDocument {
    pub id: u64,
    #[serde(default)]
    pub confidential: bool,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub description: String,
}

// ---------------------------------------------------------------- the notices

/// The text of the first `<name …>…</name>` in `s`, unescaped.
fn tag(s: &str, name: &str) -> Option<String> {
    tag_raw(s, name).map(unescape)
}

/// The same, still escaped: for an element whose children are read on.
fn tag_raw<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}");
    let close = format!("</{name}>");
    let mut from = 0;
    while let Some(i) = s[from..].find(&open) {
        let at = from + i + open.len();
        // `<dcterms:type` must not match `<dcterms:typeX`.
        match s[at..].chars().next() {
            Some('>' | ' ' | '/') => {}
            _ => {
                from = at;
                continue;
            }
        }
        let body_start = at + s[at..].find('>')? + 1;
        if s[..body_start].ends_with("/>") {
            return Some("");
        }
        let end = body_start + s[body_start..].find(&close)?;
        return Some(&s[body_start..end]);
    }
    None
}

/// Every `<name …>…</name>` in `s`, still escaped.
fn tags_raw<'a>(s: &'a str, name: &str) -> Vec<&'a str> {
    let close = format!("</{name}>");
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(body) = tag_raw(rest, name) {
        out.push(body);
        // `body` is a slice of `rest`: where it ends is its offset plus length.
        let after = body.as_ptr() as usize - rest.as_ptr() as usize + body.len();
        rest = rest[after..]
            .find(&close)
            .map_or("", |j| &rest[after + j + close.len()..]);
    }
    out
}

fn unescape(s: &str) -> String {
    quick_xml::escape::unescape(s).map_or_else(|_| s.to_string(), |c| c.trim().to_string())
}

/// `lat lng` or `lat,lng`, the forms KOOP writes a point in.
fn lat_lng_pair(s: &str) -> Option<[f64; 2]> {
    let mut it = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .map(str::parse::<f64>);
    let lat = it.next()?.ok()?;
    let lng = it.next()?.ok()?;
    (it.next().is_none() && (50.0..54.0).contains(&lat) && (3.0..8.0).contains(&lng))
        .then_some([lat, lng])
}

/// Every coordinate pair of a WKT geometry in WGS84, as `[lat, lng]`. WKT puts
/// longitude first.
fn wkt_points(wkt: &str) -> Vec<[f64; 2]> {
    let body: String = wkt
        .chars()
        .skip_while(|c| *c != '(')
        .map(|c| if c == '(' || c == ')' { ' ' } else { c })
        .collect();
    body.split(',')
        .filter_map(|pair| {
            let mut it = pair.split_whitespace().map(str::parse::<f64>);
            let lng = it.next()?.ok()?;
            let lat = it.next()?.ok()?;
            ((50.0..54.0).contains(&lat) && (3.0..8.0).contains(&lng)).then_some([lat, lng])
        })
        .collect()
}

/// The centre of a ring's bounding box: inside any area a notice draws, which
/// a vertex mean of a lopsided polygon need not be close to.
fn middle(points: &[[f64; 2]]) -> Option<[f64; 2]> {
    let first = points.first()?;
    let (mut s, mut w, mut n, mut e) = (first[0], first[1], first[0], first[1]);
    for p in points {
        s = s.min(p[0]);
        n = n.max(p[0]);
        w = w.min(p[1]);
        e = e.max(p[1]);
    }
    Some([(s + n) / 2.0, (w + e) / 2.0])
}

/// Keep at most [`MAX_AREA_POINTS`] of a ring, evenly.
fn thin(points: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    if points.len() <= MAX_AREA_POINTS {
        return points;
    }
    let step = points.len() as f64 / MAX_AREA_POINTS as f64;
    (0..MAX_AREA_POINTS)
        .map(|i| points[(i as f64 * step) as usize])
        .collect()
}

/// Round to 6 decimals, ~0.1 m: the file is mostly coordinates.
fn r6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// The notices in one SRU answer. Records without a place, or from another
/// publication than the Gemeenteblad, are left out.
#[must_use]
pub fn parse_sru(xml: &str) -> Vec<RawNotice> {
    tags_raw(xml, "sru:record")
        .into_iter()
        .filter_map(parse_record)
        .collect()
}

fn parse_record(r: &str) -> Option<RawNotice> {
    if tag(r, "overheidwetgeving:publicatienaam").as_deref() != Some("Gemeenteblad") {
        return None;
    }
    let id = tag(r, "dcterms:identifier")?;
    let title = tag(r, "dcterms:title")?;
    let koop_type = tag(r, "dcterms:type").unwrap_or_default();
    let date = tag(r, "dcterms:date").or_else(|| tag(r, "dcterms:available"))?;
    let respond_until =
        tag(r, "overheidwetgeving:datumEindeReactietermijn").filter(|d| d.len() == 10);
    // The first marked place with coordinates in WGS84.
    for marking in tags_raw(r, "overheidwetgeving:gebiedsmarkering") {
        let mut area: Vec<[f64; 2]> = Vec::new();
        for g in tags_raw(marking, "overheidwetgeving:locatiegebied") {
            let g = unescape(g);
            if g.contains('(') {
                area.extend(wkt_points(&g));
            } else if let Some(p) = lat_lng_pair(&g) {
                area.push(p);
            }
        }
        let point = tag(marking, "overheidwetgeving:locatiepunt")
            .and_then(|p| lat_lng_pair(&p))
            .or_else(|| middle(&area));
        let Some([lat, lng]) = point else { continue };
        let place = tag(marking, "overheidwetgeving:geometrielabel").unwrap_or_else(|| {
            let street = tag(marking, "overheidwetgeving:straatnaam").unwrap_or_default();
            let number = tag(marking, "overheidwetgeving:huisnummer").unwrap_or_default();
            let letter = tag(marking, "overheidwetgeving:huisletter").unwrap_or_default();
            let town = tag(marking, "overheidwetgeving:woonplaats").unwrap_or_default();
            let addr = format!("{street} {number}{letter}");
            match (addr.trim(), town.as_str()) {
                ("", _) => String::new(),
                (a, "") => a.to_string(),
                (a, t) => format!("{a}, {t}"),
            }
        });
        // A single point is not an area.
        let area = if area.len() >= 3 {
            thin(area)
        } else {
            Vec::new()
        };
        return Some(RawNotice {
            id,
            koop_type,
            title,
            date,
            respond_until,
            place,
            lat,
            lng,
            area,
        });
    }
    None
}

// ---------------------------------------------------------------- the names

/// The gazetteer from PDOK's answers: names that occur in more than one town
/// keep the city of Groningen's, or are dropped if none is in the city — a
/// "Hoofdweg" in three villages cannot be placed from a title.
#[must_use]
pub fn gazetteer(docs: &[serde_json::Value]) -> Vec<PlaceName> {
    let mut by_name: HashMap<String, Vec<(String, [f64; 2])>> = HashMap::new();
    for d in docs {
        let name = ["straatnaam", "buurtnaam", "wijknaam"]
            .iter()
            .find_map(|k| d.get(*k).and_then(|v| v.as_str()))
            .map(str::to_string);
        let town = d
            .get("woonplaatsnaam")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let at = d
            .get("centroide_ll")
            .and_then(|v| v.as_str())
            .map(wkt_points)
            .and_then(|p| p.first().copied());
        if let (Some(name), Some(at)) = (name, at) {
            by_name.entry(name).or_default().push((town, at));
        }
    }
    let mut out: Vec<PlaceName> = by_name
        .into_iter()
        .filter(|(name, _)| name.chars().count() >= 5 && !NOT_PLACES.contains(&name.as_str()))
        .filter_map(|(name, places)| {
            let towns: HashSet<&str> = places.iter().map(|(t, _)| t.as_str()).collect();
            let at = if towns.len() <= 1 {
                places[0].1
            } else {
                places.iter().find(|(t, _)| t == MUNICIPALITY)?.1
            };
            Some(PlaceName {
                name,
                lat: r6(at[0]),
                lng: r6(at[1]),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The gazetteer names standing in `text` as whole words, longest first, a
/// shorter one dropped where it lies inside a longer one ("Markt" inside
/// "Grote Markt").
#[must_use]
pub fn names_in<'a>(text: &str, names: &'a [PlaceName]) -> Vec<&'a PlaceName> {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '-');
    let mut hits: Vec<(usize, usize, &PlaceName)> = Vec::new();
    for n in names {
        let mut from = 0;
        while let Some(i) = text[from..].find(&n.name) {
            let start = from + i;
            let end = start + n.name.len();
            if !word(text[..start].chars().next_back()) && !word(text[end..].chars().next()) {
                hits.push((start, end, n));
                break;
            }
            from = end;
        }
    }
    hits.sort_by_key(|(s, e, _)| (std::cmp::Reverse(e - s), *s));
    let mut kept: Vec<(usize, usize, &PlaceName)> = Vec::new();
    for h in hits {
        if !kept.iter().any(|k| h.0 >= k.0 && h.1 <= k.1) {
            kept.push(h);
        }
    }
    kept.into_iter().map(|(_, _, n)| n).collect()
}

// ---------------------------------------------------------------- the feed

/// The council's business, one [`CouncilItem`] per item title, placed by the
/// names in its title. Items naming no place are left out.
#[must_use]
pub fn council_items(meetings: &[Meeting], names: &[PlaceName]) -> Vec<CouncilItem> {
    let mut items: HashMap<String, CouncilItem> = HashMap::new();
    let mut meetings: Vec<&Meeting> = meetings
        .iter()
        .filter(|m| !m.confidential && COUNCIL_BODIES.contains(&m.dmu.name.as_str()))
        .collect();
    meetings.sort_by(|a, b| a.date.cmp(&b.date));
    for m in meetings {
        let date: String = m.date.chars().take(10).collect();
        for it in &m.items {
            let key = council_key(&it.title);
            let places = names_in(&key, names);
            if places.is_empty() {
                continue;
            }
            let item = items.entry(key.clone()).or_insert_with(|| CouncilItem {
                title: key.clone(),
                steps: Vec::new(),
                documents: Vec::new(),
                decided: false,
                places: places
                    .iter()
                    .map(|p| CouncilPlace {
                        name: p.name.clone(),
                        lat: p.lat,
                        lng: p.lng,
                    })
                    .collect(),
            });
            if !item.steps.iter().any(|s| s.url == m.full_url) {
                item.steps.push(CouncilStep {
                    date: date.clone(),
                    body: m.dmu.name.clone(),
                    url: m.full_url.clone(),
                });
            }
            for d in it.documents.iter().filter(|d| !d.confidential) {
                let name = if d.description.is_empty() {
                    d.filename.trim_end_matches(".pdf").to_string()
                } else {
                    d.description.clone()
                };
                if item.documents.iter().any(|x| x.name == name) {
                    continue;
                }
                item.decided |= name.starts_with("Raadsbesluit");
                item.documents.push(CouncilDocument {
                    name,
                    url: format!("{COUNCIL_SITE}/api/v1/meetings/{}/documents/{}", m.id, d.id),
                });
            }
        }
    }
    let mut out: Vec<CouncilItem> = items.into_values().collect();
    out.sort_by(|a, b| b.last_date().cmp(a.last_date()).then(a.title.cmp(&b.title)));
    out
}

/// The feed: notices classified, trimmed and linked to the council items that
/// name a place their title or place names, newest first.
#[must_use]
pub fn build_feed(
    raw: Vec<RawNotice>,
    meetings: &[Meeting],
    names: &[PlaceName],
    built_unix: u64,
) -> PlansFeed {
    let council = council_items(meetings, names);
    // Which council items name each place.
    let mut by_name: HashMap<&str, Vec<u32>> = HashMap::new();
    for (i, c) in council.iter().enumerate() {
        for p in &c.places {
            by_name.entry(p.name.as_str()).or_default().push(i as u32);
        }
    }
    let mut seen = HashSet::new();
    let mut notices: Vec<Notice> = raw
        .into_iter()
        .filter(|r| seen.insert(r.id.clone()))
        .map(|r| {
            let words = format!("{} {}", r.title, r.place);
            let mut linked: Vec<u32> = names_in(&words, names)
                .iter()
                .flat_map(|n| by_name.get(n.name.as_str()).cloned().unwrap_or_default())
                .collect();
            // `council` is newest first, so the lowest index is the latest.
            linked.sort_unstable();
            linked.dedup();
            linked.truncate(MAX_COUNCIL_PER_NOTICE);
            Notice {
                kind: NoticeKind::classify(&r.koop_type, &r.title),
                title: short_title(&r.title),
                id: r.id,
                place: r.place,
                date: r.date,
                respond_until: r.respond_until,
                lat: r6(r.lat),
                lng: r6(r.lng),
                area: r.area.iter().map(|p| [r6(p[0]), r6(p[1])]).collect(),
                council: linked,
            }
        })
        .collect();
    notices.sort_by(|a, b| b.date.cmp(&a.date).then(a.id.cmp(&b.id)));
    let mut bbox = [90.0, 180.0, -90.0, -180.0];
    for n in &notices {
        bbox[0] = f64::min(bbox[0], n.lat);
        bbox[1] = f64::min(bbox[1], n.lng);
        bbox[2] = f64::max(bbox[2], n.lat);
        bbox[3] = f64::max(bbox[3], n.lng);
    }
    PlansFeed {
        municipality: MUNICIPALITY.to_string(),
        built_unix,
        notices,
        council,
        bbox,
    }
}

/// `YYYY-MM-DD` for a time, UTC.
#[must_use]
pub fn ymd(t: SystemTime) -> String {
    cartopolis_core::plans::civil_date(
        t.duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
}

// ---------------------------------------------------------------- fetching

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent("cartopolis-atlas (+https://cartopolis.org)")
        .timeout(Duration::from_secs(120))
        .build()
}

fn read_body(resp: ureq::Response) -> Result<String, String> {
    let mut body = String::new();
    std::io::Read::read_to_string(
        &mut std::io::Read::take(resp.into_reader(), 256 * 1024 * 1024),
        &mut body,
    )
    .map_err(|e| e.to_string())?;
    Ok(body)
}

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Every Gemeenteblad notice of the municipality since `since` (`YYYY-MM-DD`).
fn fetch_notices(agent: &ureq::Agent, since: &str) -> Result<Vec<RawNotice>, String> {
    let query = format!(
        "c.product-area==officielepublicaties AND dt.creator==\"{MUNICIPALITY}\" \
         AND w.publicatienaam==\"Gemeenteblad\" AND dt.date>={since}"
    );
    let mut out = Vec::new();
    let mut start = 1;
    loop {
        let url = format!(
            "{SRU}?query={}&maximumRecords={SRU_PAGE}&startRecord={start}",
            encode(&query)
        );
        let body = read_body(agent.get(&url).call().map_err(|e| e.to_string())?)?;
        let records = tags_raw(&body, "sru:record").len();
        out.extend(parse_sru(&body));
        let total: usize = tag(&body, "sru:numberOfRecords")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        start += SRU_PAGE;
        if records == 0 || start > total {
            break;
        }
    }
    Ok(out)
}

/// Every meeting between `from` and `to`, seconds since the epoch.
fn fetch_meetings(agent: &ureq::Agent, from: u64, to: u64) -> Result<Vec<Meeting>, String> {
    let url = format!("{COUNCIL_SITE}/api/v1/meetings?date_from={from}&date_to={to}");
    let body = read_body(agent.get(&url).call().map_err(|e| e.to_string())?)?;
    serde_json::from_str(&body).map_err(|e| format!("council meetings: {e}"))
}

/// Every street, neighbourhood and district of the municipality.
fn fetch_gazetteer(agent: &ureq::Agent) -> Result<Vec<PlaceName>, String> {
    let mut docs = Vec::new();
    let mut start = 0;
    loop {
        let url = format!(
            "{LOCATIESERVER}?q=*:*&fq=gemeentecode:{MUNICIPALITY_CODE}&fq={}&rows={PDOK_PAGE}&start={start}\
             &fl=type,straatnaam,buurtnaam,wijknaam,woonplaatsnaam,centroide_ll",
            encode("type:(weg OR buurt OR wijk)")
        );
        let body = read_body(agent.get(&url).call().map_err(|e| e.to_string())?)?;
        let v: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| format!("gazetteer: {e}"))?;
        let page = v["response"]["docs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let total = v["response"]["numFound"].as_u64().unwrap_or(0) as usize;
        let n = page.len();
        docs.extend(page);
        start += PDOK_PAGE;
        if n == 0 || start >= total {
            break;
        }
    }
    let names = gazetteer(&docs);
    if names.len() < 500 {
        return Err(format!("gazetteer holds {} names: not kept", names.len()));
    }
    Ok(names)
}

// ---------------------------------------------------------------- the mirror

/// One built feed, as served.
struct Copy {
    body: Arc<[u8]>,
    built: SystemTime,
}

/// The mirror: the feed in memory, the files it and its sources are kept in.
pub struct PlansMirror {
    dir: PathBuf,
    refresh: bool,
    current: RwLock<Option<Copy>>,
}

impl PlansMirror {
    /// A mirror keeping its files under `data_dir/plans/`. `refresh: false`
    /// serves whatever feed is on disk and never fetches, which is what the
    /// tests and a box without outbound access want.
    #[must_use]
    pub fn new(data_dir: &Path, refresh: bool) -> Self {
        let dir = data_dir.join("plans");
        let file = dir.join("groningen.json");
        let current = std::fs::read(&file).ok().and_then(|body| {
            let feed: PlansFeed = serde_json::from_slice(&body).ok()?;
            (feed.notices.len() >= MIN_NOTICES).then(|| Copy {
                built: std::fs::metadata(&file)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH),
                body: body.into(),
            })
        });
        Self {
            dir,
            refresh,
            current: RwLock::new(current),
        }
    }

    fn due_in(&self) -> Duration {
        let built = self
            .current
            .read()
            .ok()
            .and_then(|c| c.as_ref().map(|c| c.built));
        built
            .and_then(|t| t.elapsed().ok())
            .map_or(Duration::ZERO, |age| REFRESH.saturating_sub(age))
    }

    /// Write `body` beside `name` and move it over, so a reader never sees half.
    fn write(&self, name: &str, body: &[u8]) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let file = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}.part"));
        std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &file).map_err(|e| e.to_string())
    }

    fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> Option<T> {
        serde_json::from_slice(&std::fs::read(self.dir.join(name)).ok()?).ok()
    }

    /// The gazetteer: the copy on disk while it is fresh, else fetched, else the
    /// stale copy.
    fn names(&self, agent: &ureq::Agent) -> Result<Vec<PlaceName>, String> {
        let file = self.dir.join("names.json");
        let fresh = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < GAZETTEER_TTL);
        if fresh {
            if let Some(names) = self.read("names.json") {
                return Ok(names);
            }
        }
        match fetch_gazetteer(agent) {
            Ok(names) => {
                let body = serde_json::to_vec(&names).map_err(|e| e.to_string())?;
                self.write("names.json", &body)?;
                Ok(names)
            }
            Err(e) => self.read("names.json").ok_or(e),
        }
    }

    /// The council's meetings: fetched, else the last copy read.
    fn meetings(&self, agent: &ureq::Agent) -> Vec<Meeting> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let from = now.saturating_sub(COUNCIL_DAYS_BACK * 86_400);
        let to = now + COUNCIL_DAYS_AHEAD * 86_400;
        match fetch_meetings(agent, from, to) {
            Ok(m) if !m.is_empty() => {
                if let Ok(body) = serde_json::to_vec(&m) {
                    let _ = self.write("meetings.json", &body);
                }
                m
            }
            other => {
                if let Err(error) = other {
                    tracing::warn!(%error, "council meetings fetch failed; using the last copy");
                }
                self.read("meetings.json").unwrap_or_default()
            }
        }
    }

    /// Build once and, if it holds enough notices, keep it. Blocking.
    fn refresh_blocking(&self) -> Result<(usize, usize), String> {
        let agent = agent();
        let now = SystemTime::now();
        let since = ymd(now - Duration::from_secs(NOTICE_DAYS * 86_400));
        let raw = fetch_notices(&agent, &since)?;
        if raw.len() < MIN_NOTICES {
            return Err(format!(
                "{} notices since {since}, fewer than {MIN_NOTICES}: not kept",
                raw.len()
            ));
        }
        let names = self.names(&agent)?;
        let meetings = self.meetings(&agent);
        let built_unix = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let feed = build_feed(raw, &meetings, &names, built_unix);
        let counts = (feed.notices.len(), feed.council.len());
        self.store(&feed)?;
        Ok(counts)
    }

    fn store(&self, feed: &PlansFeed) -> Result<(), String> {
        let body = serde_json::to_vec(feed).map_err(|e| e.to_string())?;
        self.write("groningen.json", &body)?;
        if let Ok(mut c) = self.current.write() {
            *c = Some(Copy {
                body: body.into(),
                built: SystemTime::now(),
            });
        }
        Ok(())
    }

    /// Rebuild in the background for as long as the process runs.
    pub fn spawn_refresh(self: &Arc<Self>) {
        if !self.refresh {
            return;
        }
        let mirror = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(mirror.due_in()).await;
                let m = mirror.clone();
                let result = tokio::task::spawn_blocking(move || m.refresh_blocking())
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                match result {
                    Ok((notices, council)) => {
                        tracing::info!(notices, council, "plans feed rebuilt");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "plans feed rebuild failed; keeping the last copy");
                        tokio::time::sleep(RETRY_AFTER_FAILURE).await;
                    }
                }
            }
        });
    }
}

/// `GET /plans/groningen.json`: the current feed, or `503` before the first.
pub async fn serve(State(mirror): State<Arc<PlansMirror>>) -> Response {
    let copy = mirror
        .current
        .read()
        .ok()
        .and_then(|c| c.as_ref().map(|c| c.body.clone()));
    let Some(body) = copy else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
            "the plans feed has not been built yet",
        )
            .into_response();
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (
                header::CACHE_CONTROL,
                format!("public, max-age={CLIENT_MAX_AGE_S}"),
            ),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".to_string()),
        ],
        body.to_vec(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two records as KOOP answers them (2026-10-30), trimmed to the fields
    /// read, and one from the provincial gazette that must be left out.
    const SRU_ANSWER: &str = r#"<sru:searchRetrieveResponse><sru:numberOfRecords>3</sru:numberOfRecords><sru:records>
<sru:record><sru:recordData><gzd:gzd><gzd:originalData><overheidwetgeving:meta><overheidwetgeving:owmskern><dcterms:identifier>gmb-2026-501719</dcterms:identifier><dcterms:title>Kennisgeving aanvraag omgevingsvergunning reguliere procedure, het aanbrengen van bestrating, het plaatsen van putkasten en een boomgroeiplaats, Akerkhof, tussen 1 en 5 t/m 49 te Groningen</dcterms:title><dcterms:type scheme="OVERHEIDop.Rubriek">omgevingsvergunning</dcterms:type><dcterms:creator scheme="OVERHEID.Gemeente">Groningen</dcterms:creator></overheidwetgeving:owmskern><overheidwetgeving:owmsmantel><dcterms:date>2026-10-28</dcterms:date></overheidwetgeving:owmsmantel><overheidwetgeving:tpmeta><overheidwetgeving:datumEindeReactietermijn>2026-12-07</overheidwetgeving:datumEindeReactietermijn><overheidwetgeving:publicatienaam>Gemeenteblad</overheidwetgeving:publicatienaam><overheidwetgeving:gebiedsmarkering scheme="OVERHEIDop.locatietype"><overheidwetgeving:Vlak><overheidwetgeving:geometrie>POLYGON ((233435.441 581762.022, 233439.921 581748.582, 233522.054 581774.342, 233595.601 581802.342, 233592.241 581815.409, 233435.441 581762.022))</overheidwetgeving:geometrie><overheidwetgeving:locatiegebied>POLYGON((6.5614254 53.216725,6.5614891 53.216604,6.5627248 53.216823,6.5638324 53.217064,6.5637853 53.217182,6.5614254 53.216725))</overheidwetgeving:locatiegebied><overheidwetgeving:geometrielabel>Akerkhof, tussen 1 en 5 t/m 49 te Groningen</overheidwetgeving:geometrielabel></overheidwetgeving:Vlak></overheidwetgeving:gebiedsmarkering></overheidwetgeving:tpmeta></overheidwetgeving:meta></gzd:originalData></gzd:gzd></sru:recordData></sru:record>
<sru:record><sru:recordData><gzd:gzd><gzd:originalData><overheidwetgeving:meta><overheidwetgeving:owmskern><dcterms:identifier>gmb-2026-490665</dcterms:identifier><dcterms:title>Kennisgeving aanvraag omgevingsvergunning reguliere procedure, het vellen van 1 boom, Zwanestraat 27, 9712 CJ Groningen</dcterms:title><dcterms:type scheme="OVERHEIDop.Rubriek">omgevingsvergunning</dcterms:type></overheidwetgeving:owmskern><overheidwetgeving:owmsmantel><dcterms:date>2026-10-21</dcterms:date></overheidwetgeving:owmsmantel><overheidwetgeving:tpmeta><overheidwetgeving:datumEindeReactietermijn>2026-11-30</overheidwetgeving:datumEindeReactietermijn><overheidwetgeving:publicatienaam>Gemeenteblad</overheidwetgeving:publicatienaam><overheidwetgeving:gebiedsmarkering scheme="OVERHEIDop.locatietype"><overheidwetgeving:Adres><overheidwetgeving:geometrie>POINT(233572.0 581914.0)</overheidwetgeving:geometrie><overheidwetgeving:locatiepunt>53.2183545124236 6.56390615878306</overheidwetgeving:locatiepunt><overheidwetgeving:locatiegebied>53.218355,6.5639062</overheidwetgeving:locatiegebied><overheidwetgeving:huisnummer>27</overheidwetgeving:huisnummer><overheidwetgeving:straatnaam>Zwanestraat</overheidwetgeving:straatnaam><overheidwetgeving:woonplaats>Groningen</overheidwetgeving:woonplaats></overheidwetgeving:Adres></overheidwetgeving:gebiedsmarkering></overheidwetgeving:tpmeta></overheidwetgeving:meta></gzd:originalData></gzd:gzd></sru:recordData></sru:record>
<sru:record><sru:recordData><gzd:gzd><gzd:originalData><overheidwetgeving:meta><overheidwetgeving:owmskern><dcterms:identifier>prb-2026-1</dcterms:identifier><dcterms:title>Provinciaal besluit</dcterms:title><dcterms:type>andere beschikking</dcterms:type></overheidwetgeving:owmskern><overheidwetgeving:owmsmantel><dcterms:date>2026-10-21</dcterms:date></overheidwetgeving:owmsmantel><overheidwetgeving:tpmeta><overheidwetgeving:publicatienaam>Provinciaal blad</overheidwetgeving:publicatienaam><overheidwetgeving:gebiedsmarkering><overheidwetgeving:Punt><overheidwetgeving:locatiepunt>53.2 6.5</overheidwetgeving:locatiepunt></overheidwetgeving:Punt></overheidwetgeving:gebiedsmarkering></overheidwetgeving:tpmeta></overheidwetgeving:meta></gzd:originalData></gzd:gzd></sru:recordData></sru:record>
</sru:records></sru:searchRetrieveResponse>"#;

    fn names() -> Vec<PlaceName> {
        [
            ("Akerkhof", 53.2168, 6.5627),
            ("Vismarkt", 53.2171, 6.5651),
            ("Zwanestraat", 53.2184, 6.5640),
            ("Grote Markt", 53.2189, 6.5675),
        ]
        .into_iter()
        .map(|(name, lat, lng)| PlaceName {
            name: name.into(),
            lat,
            lng,
        })
        .collect()
    }

    fn meetings() -> Vec<Meeting> {
        serde_json::from_str(
            r#"[
            {"id":5563,"dmu":{"id":2,"name":"Politieke woensdag (Dag agenda)"},"fullUrl":"https://gemeenteraad.groningen.nl/Vergaderingen/Politieke-woensdag-Dag-agenda/2026/11-februari/09:30","confidential":false,"date":"2026-02-11 09:30:00",
             "items":[{"id":1,"title":"Uitvoeringskrediet groot onderhoud Vismarkt, Masterplan Warenmarkt (raadsvoorstel 14-1-26)","documents":[{"id":92385,"confidential":false,"filename":"Voorbereidingsmemo.pdf","description":"Voorbereidingsmemo"}]}]},
            {"id":5416,"dmu":{"id":1,"name":"gemeenteraad"},"fullUrl":"https://gemeenteraad.groningen.nl/Vergaderingen/gemeenteraad/2026/18-februari/16:30","confidential":false,"date":"2026-02-18 16:30:00",
             "items":[{"id":39883,"title":"Uitvoeringskrediet groot onderhoud Vismarkt, Masterplan Warenmarkt (raadsvoorstel 14-1-26)","documents":[{"id":93067,"confidential":false,"filename":"Raadsbesluit.pdf","description":"Raadsbesluit Uitvoeringskrediet groot onderhoud Vismarkt, Masterplan Warenmarkt (raadsvoorstel 14-1-26)"}]},
                      {"id":38365,"title":"OPENING EN MEDEDELINGEN","documents":[]}]},
            {"id":5500,"dmu":{"id":11,"name":"Agendacommissie"},"fullUrl":"x","confidential":false,"date":"2026-02-01 12:00:00",
             "items":[{"id":9,"title":"Vismarkt","documents":[]}]}
            ]"#,
        )
        .unwrap()
    }

    #[test]
    fn records_are_read_with_their_place_and_deadline() {
        let raw = parse_sru(SRU_ANSWER);
        assert_eq!(raw.len(), 2, "the provincial record is left out");
        let akerkhof = &raw[0];
        assert_eq!(akerkhof.id, "gmb-2026-501719");
        assert_eq!(akerkhof.respond_until.as_deref(), Some("2026-12-07"));
        assert_eq!(akerkhof.area.len(), 6);
        assert!((akerkhof.lat - 53.2169).abs() < 0.001, "{}", akerkhof.lat);
        assert!((akerkhof.lng - 6.5626).abs() < 0.001, "{}", akerkhof.lng);
        assert_eq!(
            akerkhof.place,
            "Akerkhof, tussen 1 en 5 t/m 49 te Groningen"
        );
        let tree = &raw[1];
        assert!((tree.lat - 53.218_354).abs() < 1e-5);
        assert!(tree.area.is_empty(), "one point is not an area");
        assert_eq!(tree.place, "Zwanestraat 27, Groningen");
    }

    #[test]
    fn a_council_item_is_one_item_through_its_meetings() {
        let items = council_items(&meetings(), &names());
        assert_eq!(items.len(), 1, "{items:?}");
        let v = &items[0];
        assert_eq!(
            v.title,
            "Uitvoeringskrediet groot onderhoud Vismarkt, Masterplan Warenmarkt"
        );
        let dates: Vec<&str> = v.steps.iter().map(|s| s.date.as_str()).collect();
        assert_eq!(
            dates,
            ["2026-02-11", "2026-02-18"],
            "the agenda committee is not a step"
        );
        assert!(v.decided);
        assert_eq!(v.places.len(), 1);
        assert_eq!(v.places[0].name, "Vismarkt");
        assert_eq!(
            v.documents[1].url,
            "https://gemeenteraad.groningen.nl/api/v1/meetings/5416/documents/93067"
        );
    }

    #[test]
    fn names_match_whole_words_and_the_longest_wins() {
        let mut n = names();
        n.push(PlaceName {
            name: "Markt".into(),
            lat: 0.0,
            lng: 0.0,
        });
        let hit: Vec<&str> = names_in("Herinrichting Grote Markt oostzijde", &n)
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(hit, ["Grote Markt"]);
        assert!(names_in("Vismarkten in Nederland", &n).is_empty());
    }

    #[test]
    fn the_feed_links_a_notice_to_the_council_by_place() {
        let mut n = names();
        // The council item names the Vismarkt; the Akerkhof notice is linked
        // through a council title naming both.
        let mut m = meetings();
        m[1].items[0].title = "Groot onderhoud Vismarkt en Akerkhof".into();
        n.retain(|p| p.name != "Grote Markt");
        let feed = build_feed(parse_sru(SRU_ANSWER), &m, &n, 0);
        assert_eq!(feed.notices.len(), 2);
        assert_eq!(feed.notices[0].id, "gmb-2026-501719", "newest first");
        assert_eq!(feed.notices[0].kind, NoticeKind::Application);
        assert!(feed.notices[0]
            .title
            .starts_with("Het aanbrengen van bestrating"));
        let linked = &feed.notices[0].council;
        assert_eq!(linked.len(), 1);
        assert_eq!(
            feed.council[linked[0] as usize].title,
            "Groot onderhoud Vismarkt en Akerkhof"
        );
        assert!(
            feed.notices[1].council.is_empty(),
            "no item names the Zwanestraat"
        );
        assert!(feed.covers(53.2175, 6.563));
        assert!(!feed.covers(52.37, 4.89));
    }

    #[test]
    fn ambiguous_names_keep_the_city_or_go() {
        let docs: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
            {"type":"weg","straatnaam":"Hoofdweg","woonplaatsnaam":"Harkstede","centroide_ll":"POINT(6.69 53.21)"},
            {"type":"weg","straatnaam":"Hoofdweg","woonplaatsnaam":"Ten Boer","centroide_ll":"POINT(6.70 53.27)"},
            {"type":"weg","straatnaam":"Oosterstraat","woonplaatsnaam":"Groningen","centroide_ll":"POINT(6.5707 53.2153)"},
            {"type":"weg","straatnaam":"Oosterstraat","woonplaatsnaam":"Ten Boer","centroide_ll":"POINT(6.70 53.27)"},
            {"type":"buurt","buurtnaam":"Binnenstad-Oost","woonplaatsnaam":"Groningen","centroide_ll":"POINT(6.57 53.218)"},
            {"type":"weg","straatnaam":"Kade","woonplaatsnaam":"Groningen","centroide_ll":"POINT(6.57 53.218)"}
            ]"#,
        )
        .unwrap();
        let g = gazetteer(&docs);
        let names: Vec<&str> = g.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Binnenstad-Oost", "Oosterstraat"]);
        assert!((g[1].lng - 6.5707).abs() < 1e-9, "the city's Oosterstraat");
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(ymd(SystemTime::UNIX_EPOCH), "1970-01-01");
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_318_400);
        assert_eq!(ymd(t), "2026-10-30");
    }

    /// The whole build against the live sources, for a person to read:
    /// `cargo test -p cartopolis_atlas plans::tests::live -- --ignored --nocapture`.
    #[test]
    #[ignore = "reaches KOOP, the council site and PDOK"]
    fn live() {
        let dir = std::env::var("CARTO_PLANS_LIVE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("plans-live"));
        let mirror = PlansMirror::new(&dir, true);
        let (notices, council) = mirror.refresh_blocking().expect("built");
        let feed: PlansFeed =
            serde_json::from_slice(&std::fs::read(dir.join("plans/groningen.json")).unwrap())
                .unwrap();
        let linked = feed
            .notices
            .iter()
            .filter(|n| !n.council.is_empty())
            .count();
        let open = feed
            .notices
            .iter()
            .filter(|n| n.open_on(&ymd(SystemTime::now())))
            .count();
        let bytes = std::fs::metadata(dir.join("plans/groningen.json"))
            .unwrap()
            .len();
        println!(
            "notices {notices}, open {open}, linked {linked}, council {council}, {bytes} bytes"
        );
        for c in feed.council.iter().take(25) {
            let names: Vec<&str> = c.places.iter().map(|p| p.name.as_str()).collect();
            println!("  {} | {} | {names:?}", c.last_date(), c.title);
        }
        assert!(notices >= MIN_NOTICES);
    }

    #[test]
    fn a_small_feed_is_not_loaded_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mirror = PlansMirror::new(dir.path(), false);
        mirror
            .store(&build_feed(parse_sru(SRU_ANSWER), &[], &names(), 0))
            .unwrap();
        assert!(PlansMirror::new(dir.path(), false)
            .current
            .read()
            .unwrap()
            .is_none());
    }
}
