//! **Official areas as scenario boards** (`cartopolis_scenario::board`): a
//! publisher's polygons and figures, simplified, linked to the rung above and
//! written as the compact area files the client reads layers from
//! (`docs/notes/scenarios.md`).
//!
//! The sets, `atlas areas <set> --out <dir>`:
//!
//! - **`cbs-2024`**: CBS's Kerncijfers wijken en buurten 2024 from PDOK — buurt,
//!   wijk and gemeente with their figures, each linked to its parent by CBS's
//!   own codes; the provinces from PDOK's bestuurlijke gebieden, a gemeente
//!   placed in one by a point inside it; and the country. CBS publishes no
//!   province or country figures here, so those are rolled up from the
//!   gemeenten (`AreaSet::roll_up`). Five files, `cbs-<rung>-2024.bin`.
//! - **`ses-2023`**: socio-economic status per buurt, wijk and gemeente, 2014
//!   to 2023 — CBS's SES-WOA, the successor of SCP's statusscores — on the
//!   same 2024 boundaries; province and country rolled up by households.
//! - **`pbl-2050`**: PBL and CBS's regional population forecast 2023–2050
//!   with its 67 % interval, per gemeente, provincie and the country.
//! - **`wlo-2025`**: the WLO 2025 regional futures (PBL with CPB), High and
//!   Low, per COROP region for 2021, 2040, 2050 and 2060.
//! - **`ne-admin1-europe`**: Natural Earth's admin-1 over Europe, no figures;
//!   a province rung for scenarios beyond the Netherlands.
//! - **`ne-admin0-europe`**: Natural Earth's countries over Europe, no
//!   figures; the outline the painted far map borders the home country with.
//!
//! A figure over time is a **series**: one field per year under one key
//! (`AreaSet::field_at`), which a scenario's clock of years plays through.
//!
//! These are whole-country and whole-continent files, unlike `atlas run`'s
//! per-area datasets in RD, so they are their own command. The output goes in
//! the served site tree under `areas/`, where the client fetches it.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context};
use cartopolis_scenario::board::{simplify_ring, Area, AreaSet, Field, LonLat, Roll};
use cartopolis_scenario::ladder::Rung;
use serde_json::Value;

/// Natural Earth's admin-1 as GeoJSON, in the project's own repository.
pub const NATURAL_EARTH_ADMIN1: &str = "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_10m_admin_1_states_provinces.geojson";
/// Natural Earth's countries, from the same repository.
pub const NATURAL_EARTH_ADMIN0: &str = "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_10m_admin_0_countries.geojson";

/// CBS's wijken en buurten 2024, as PDOK serves them.
const CBS_WFS: &str = "https://service.pdok.nl/cbs/wijkenbuurten/2024/wfs/v1_0";
/// The Kadaster's provinces and country outline.
const BESTUURLIJK_WFS: &str = "https://service.pdok.nl/kadaster/bestuurlijkegebieden/wfs/v1_0";
/// PDOK answers at most this many features a page.
const WFS_PAGE: usize = 1000;

const CBS_ATTRIBUTION: &str =
    "Figures: CBS, Kerncijfers wijken en buurten 2024 · areas: CBS, Kadaster, via PDOK";

fn read_source(from: &str) -> anyhow::Result<Value> {
    let mut text = String::new();
    if from.starts_with("http://") || from.starts_with("https://") {
        ureq::get(from)
            .call()
            .with_context(|| format!("fetching {from}"))?
            .into_reader()
            .take(1024 * 1024 * 1024)
            .read_to_string(&mut text)?;
    } else {
        text = std::fs::read_to_string(from).with_context(|| format!("reading {from}"))?;
    }
    Ok(serde_json::from_str(&text)?)
}

/// Every feature of a WFS layer, in WGS84, a page at a time.
fn wfs_features(base: &str, type_name: &str) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    loop {
        let url = format!(
            "{base}?request=GetFeature&service=WFS&version=2.0.0&typeNames={type_name}\
             &outputFormat=application/json&srsName=EPSG:4326&count={WFS_PAGE}&startIndex={}",
            out.len()
        );
        let page = read_source(&url)?;
        let features = page["features"].as_array().cloned().unwrap_or_default();
        let n = features.len();
        out.extend(features);
        eprintln!("  {type_name}: {}", out.len());
        if n < WFS_PAGE {
            return Ok(out);
        }
    }
}

fn first_text(props: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|k| props.get(*k)?.as_str())
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}

fn ring(v: &Value) -> Vec<LonLat> {
    let mut r: Vec<LonLat> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some([p.get(0)?.as_f64()?, p.get(1)?.as_f64()?]))
        .collect();
    // GeoJSON closes a ring by repeating its first point; the area file does not.
    if r.len() > 1 && r.first() == r.last() {
        r.pop();
    }
    r
}

/// A `Polygon`/`MultiPolygon` as polygons of rings.
fn polygons(geometry: &Value) -> Vec<Vec<Vec<LonLat>>> {
    let coords = &geometry["coordinates"];
    let poly = |p: &Value| {
        p.as_array()
            .into_iter()
            .flatten()
            .map(ring)
            .collect::<Vec<_>>()
    };
    match geometry["type"].as_str() {
        Some("Polygon") => vec![poly(coords)],
        Some("MultiPolygon") => coords.as_array().into_iter().flatten().map(poly).collect(),
        _ => Vec::new(),
    }
}

fn simplified(polys: &[Vec<Vec<LonLat>>], tolerance: f64) -> Vec<Vec<Vec<LonLat>>> {
    polys
        .iter()
        .map(|rings| {
            rings
                .iter()
                .map(|r| simplify_ring(r, tolerance))
                .filter(|r| r.len() >= 3)
                .collect::<Vec<_>>()
        })
        .filter(|rings| !rings.is_empty())
        .collect()
}

// ─── Natural Earth ──────────────────────────────────────────────────────────

/// Natural Earth admin-1 over Europe (lon −25…45, lat 34…72), ~1 km tolerance.
pub fn natural_earth_europe(source: &Value) -> anyhow::Result<AreaSet> {
    Ok(AreaSet::new(
        "ne-admin1-europe".into(),
        "Provinces: Natural Earth (public domain)".into(),
        natural_earth_areas(source, &["adm1_code"], 0.01)?,
    )
    .with_rung(Rung::Province))
}

/// Natural Earth's countries over Europe, ~200 m tolerance: the outline the
/// painted far map borders the home country with (`systems::home_country`),
/// which follows the coast and takes in the islands, where the Kadaster's
/// country runs out to sea with the municipalities.
pub fn natural_earth_countries_europe(source: &Value) -> anyhow::Result<AreaSet> {
    Ok(AreaSet::new(
        "ne-admin0-europe".into(),
        "Countries: Natural Earth (public domain)".into(),
        natural_earth_areas(source, &["ADM0_A3", "adm0_a3"], 0.002)?,
    )
    .with_rung(Rung::Country))
}

/// The features of a Natural Earth layer over Europe, each named by its
/// `id_keys` and simplified to `tolerance` degrees.
fn natural_earth_areas(
    source: &Value,
    id_keys: &[&str],
    tolerance: f64,
) -> anyhow::Result<Vec<Area>> {
    let [w, s, e, n] = [-25.0, 34.0, 45.0, 72.0];
    let mut areas = Vec::new();
    for f in source["features"].as_array().into_iter().flatten() {
        let props = &f["properties"];
        let (Some(id), Some(name), Some(country)) = (
            first_text(props, id_keys),
            first_text(props, &["name_en", "NAME_EN", "name", "NAME"]),
            first_text(props, &["adm0_a3", "ADM0_A3"]),
        ) else {
            continue;
        };
        let polys = simplified(&polygons(&f["geometry"]), tolerance);
        if polys.is_empty() {
            continue;
        }
        let area = Area::new(id, name, country, polys);
        let b = area.bbox;
        if b[2] < w || b[0] > e || b[3] < s || b[1] > n {
            continue;
        }
        areas.push(area);
    }
    if areas.is_empty() {
        bail!("no areas in Europe");
    }
    areas.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(areas)
}

// ─── CBS ────────────────────────────────────────────────────────────────────

/// One CBS figure: its property in the WFS and what the area file calls it.
struct CbsField {
    property: &'static str,
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    /// How it rolls up; `None` for a count (summed), else the weight's key.
    weight: Option<&'static str>,
}

/// The figures carried, a handful of the Kerncijfers' hundred, chosen to show
/// each kind of roll-up: counts, shares of people, of households and of homes,
/// a mean, and a density.
const CBS_FIELDS: &[CbsField] = &[
    CbsField {
        property: "aantalInwoners",
        key: "people",
        label: "Inhabitants",
        unit: "people",
        weight: None,
    },
    CbsField {
        property: "aantalHuishoudens",
        key: "households",
        label: "Households",
        unit: "households",
        weight: None,
    },
    CbsField {
        property: "woningvoorraad",
        key: "homes",
        label: "Homes",
        unit: "homes",
        weight: None,
    },
    CbsField {
        property: "oppervlakteLandInHa",
        key: "land_ha",
        label: "Land area",
        unit: "ha",
        weight: None,
    },
    CbsField {
        property: "bevolkingsdichtheidInwonersPerKm2",
        key: "density",
        label: "People per km²",
        unit: "/km²",
        weight: Some("land_ha"),
    },
    CbsField {
        property: "percentagePersonen0Tot15Jaar",
        key: "share_0_15",
        label: "Aged under 15",
        unit: "%",
        weight: Some("people"),
    },
    CbsField {
        property: "percentagePersonen65JaarEnOuder",
        key: "share_65_plus",
        label: "Aged 65 and over",
        unit: "%",
        weight: Some("people"),
    },
    CbsField {
        property: "percentageEenpersoonshuishoudens",
        key: "single_households",
        label: "Single-person households",
        unit: "%",
        weight: Some("households"),
    },
    CbsField {
        property: "percentageKoopwoningen",
        key: "owner_occupied",
        label: "Owner-occupied homes",
        unit: "%",
        weight: Some("homes"),
    },
    CbsField {
        property: "gemiddeldeWoningwaarde",
        key: "home_value",
        label: "Average home value",
        unit: "k€",
        weight: Some("homes"),
    },
];

fn cbs_fields() -> Vec<Field> {
    CBS_FIELDS
        .iter()
        .map(|f| Field {
            key: f.key.into(),
            label: f.label.into(),
            unit: f.unit.into(),
            roll: f
                .weight
                .map_or(Roll::Sum, |w| Roll::Mean { weight: w.into() }),
            year: Some(2024),
        })
        .collect()
}

/// A CBS figure: a negative number is one of CBS's codes for "kept back" or
/// "not applicable" (−99997, −99995 …), which is no figure at all.
fn cbs_values(props: &Value) -> Vec<f32> {
    CBS_FIELDS
        .iter()
        .map(|f| match props.get(f.property).and_then(Value::as_f64) {
            Some(v) if v >= 0.0 => v as f32,
            _ => f32::NAN,
        })
        .collect()
}

/// One CBS level: the land parts of its areas, with figures and parents.
fn cbs_level(
    features: &[Value],
    name: &str,
    rung: Rung,
    id_key: &str,
    name_key: &str,
    parent_key: Option<&str>,
    tolerance: f64,
) -> AreaSet {
    let mut set = cbs_outlines(
        features, name, rung, id_key, name_key, parent_key, tolerance,
    );
    let by_id: HashMap<String, Vec<f32>> = features
        .iter()
        .filter(|f| f["properties"].get("water").and_then(Value::as_str) == Some("NEE"))
        .filter_map(|f| {
            Some((
                first_text(&f["properties"], &[id_key])?,
                cbs_values(&f["properties"]),
            ))
        })
        .collect();
    set.fields = cbs_fields();
    for a in &mut set.areas {
        a.values = by_id
            .get(&a.id)
            .cloned()
            .unwrap_or_else(|| vec![f32::NAN; set.fields.len()]);
    }
    set
}

/// One CBS level's areas, outlines and parents only: the board other
/// publishers' figures for the same boundaries are put on.
fn cbs_outlines(
    features: &[Value],
    name: &str,
    rung: Rung,
    id_key: &str,
    name_key: &str,
    parent_key: Option<&str>,
    tolerance: f64,
) -> AreaSet {
    let mut areas = Vec::new();
    for f in features {
        let p = &f["properties"];
        // Each area comes as its land and its water part; the figures are the
        // land part's, and water is not where people live.
        if p.get("water").and_then(Value::as_str) != Some("NEE") {
            continue;
        }
        let (Some(id), Some(label)) = (first_text(p, &[id_key]), first_text(p, &[name_key])) else {
            continue;
        };
        let polys = simplified(&polygons(&f["geometry"]), tolerance);
        if polys.is_empty() {
            continue;
        }
        let mut area = Area::new(id, label, "NLD".into(), polys);
        area.parent = parent_key.and_then(|k| first_text(p, &[k]));
        areas.push(area);
    }
    areas.sort_by(|a, b| a.id.cmp(&b.id));
    AreaSet::new(name.into(), CBS_ATTRIBUTION.into(), areas).with_rung(rung)
}

/// Place each area of `child` in the area of `parent` that holds a point
/// inside it, where it has no parent yet.
fn place_in(child: &mut AreaSet, parent: &AreaSet) -> usize {
    let mut missed = 0;
    for a in &mut child.areas {
        if a.parent.is_some() {
            continue;
        }
        a.parent = a
            .inner_point()
            .and_then(|p| parent.area_at(p))
            .map(|i| parent.areas[i].id.clone());
        missed += usize::from(a.parent.is_none());
    }
    missed
}

/// The five CBS sets, finest first.
pub fn cbs_2024() -> anyhow::Result<Vec<AreaSet>> {
    let buurten = wfs_features(CBS_WFS, "wijkenbuurten:buurten")?;
    let wijken = wfs_features(CBS_WFS, "wijkenbuurten:wijken")?;
    let gemeenten = wfs_features(CBS_WFS, "wijkenbuurten:gemeenten")?;
    let provincies = wfs_features(BESTUURLIJK_WFS, "bestuurlijkegebieden:Provinciegebied")?;
    let land = wfs_features(BESTUURLIJK_WFS, "bestuurlijkegebieden:Landgebied")?;

    let buurt = cbs_level(
        &buurten,
        "cbs-buurt-2024",
        Rung::Neighbourhood,
        "buurtcode",
        "buurtnaam",
        Some("wijkcode"),
        0.000_1,
    );
    let wijk = cbs_level(
        &wijken,
        "cbs-wijk-2024",
        Rung::District,
        "wijkcode",
        "wijknaam",
        Some("gemeentecode"),
        0.000_2,
    );
    let mut gemeente = cbs_level(
        &gemeenten,
        "cbs-gemeente-2024",
        Rung::Municipality,
        "gemeentecode",
        "gemeentenaam",
        None,
        0.000_5,
    );

    let (mut provincie, mut country) =
        provinces_and_country(&mut gemeente, &provincies, &land, "cbs", CBS_ATTRIBUTION);
    provincie.roll_up(&gemeente);
    country.roll_up(&provincie);

    Ok(vec![buurt, wijk, gemeente, provincie, country])
}

/// The provinces and the country as boards above `gemeente`, named
/// `<prefix>-provincie-2024` and `<prefix>-land-2024`, without figures. Each
/// gemeente is placed in the province that holds a point inside it.
fn provinces_and_country(
    gemeente: &mut AreaSet,
    provincies: &[Value],
    land: &[Value],
    prefix: &str,
    attribution: &str,
) -> (AreaSet, AreaSet) {
    // Provinces placed against their full outlines, then simplified.
    let province_areas = |tolerance: Option<f64>| -> Vec<Area> {
        provincies
            .iter()
            .filter_map(|f| {
                let p = &f["properties"];
                let polys = polygons(&f["geometry"]);
                let polys = tolerance.map_or(polys.clone(), |t| simplified(&polys, t));
                let mut a = Area::new(
                    first_text(p, &["identificatie"])?,
                    first_text(p, &["naam"])?,
                    "NLD".into(),
                    polys,
                );
                a.parent = Some("NL".into());
                Some(a)
            })
            .collect()
    };
    let full = AreaSet::new(String::new(), String::new(), province_areas(None));
    let missed = place_in(gemeente, &full);
    if missed > 0 {
        eprintln!("  {missed} gemeenten in no province");
    }
    let provincie = AreaSet::new(
        format!("{prefix}-provincie-2024"),
        attribution.into(),
        province_areas(Some(0.001)),
    )
    .with_rung(Rung::Province);
    (
        provincie,
        country_set(land, &format!("{prefix}-land-2024"), attribution),
    )
}

/// The Netherlands as one area, code `NL`, from the Kadaster's land outline.
fn country_set(land: &[Value], name: &str, attribution: &str) -> AreaSet {
    let outline: Vec<Vec<Vec<LonLat>>> = land
        .iter()
        .flat_map(|f| simplified(&polygons(&f["geometry"]), 0.002))
        .collect();
    AreaSet::new(
        name.into(),
        attribution.into(),
        vec![Area::new(
            "NL".into(),
            "Nederland".into(),
            "NLD".into(),
            outline,
        )],
    )
    .with_rung(Rung::Country)
}

// ─── Figures over time: SCP's statuses, PBL's forecast, the WLO futures ─────

/// CBS's open-data service, v3: a table's rows, at most 10,000 a page.
const CBS_ODATA: &str = "https://opendata.cbs.nl/ODataApi/odata";

/// Every row of a CBS table, `select`ed down. The service refuses a query
/// that would return 10,000 rows or more (with a 500), and pages no further,
/// so the table is asked for by the start of its region code (`dim`), from
/// `prefixes` down: a prefix refused is asked again as its ten longer ones
/// (`BU` → `BU0` … `BU9`).
fn cbs_rows(
    table: &str,
    select: &[&str],
    dim: &str,
    prefixes: &[&str],
) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut queue: Vec<String> = prefixes.iter().rev().map(|p| (*p).to_owned()).collect();
    while let Some(prefix) = queue.pop() {
        let url = format!(
            "{CBS_ODATA}/{table}/TypedDataSet?$format=json&$select={}&$filter=startswith({dim},%27{prefix}%27)",
            select.join(",")
        );
        match ureq::get(&url).call() {
            Ok(r) => {
                let page: Value = serde_json::from_reader(r.into_reader())
                    .with_context(|| format!("reading {url}"))?;
                out.extend(page["value"].as_array().cloned().unwrap_or_default());
                eprintln!("  {table} {prefix}: {}", out.len());
            }
            Err(ureq::Error::Status(500, _)) if prefix.len() < 10 => {
                queue.extend((0..10).rev().map(|d| format!("{prefix}{d}")));
            }
            Err(e) => return Err(e).with_context(|| format!("fetching {url}")),
        }
    }
    Ok(out)
}

/// A CBS period key's year: `"2023JJ00"` → 2023.
fn period_year(row: &Value, key: &str) -> Option<i32> {
    row.get(key)?.as_str()?.get(..4)?.parse().ok()
}

/// A CBS region key, without the padding the service gives it.
fn region(row: &Value, key: &str) -> Option<String> {
    Some(row.get(key)?.as_str()?.trim().to_owned())
}

/// A field per year for each of `keys`, in that order, year by year.
fn series(keys: &[(&str, &str, &str, Roll)], years: &[i32]) -> Vec<Field> {
    years
        .iter()
        .flat_map(|&y| {
            keys.iter().map(move |(key, label, unit, roll)| Field {
                key: (*key).into(),
                label: (*label).into(),
                unit: (*unit).into(),
                roll: roll.clone(),
                year: Some(y),
            })
        })
        .collect()
}

/// Give a set its fields and each area its values by code, NaN where the
/// table has none for it.
fn fill(set: &mut AreaSet, fields: Vec<Field>, values: &HashMap<String, Vec<f32>>) {
    for a in &mut set.areas {
        a.values = values
            .get(&a.id)
            .cloned()
            .unwrap_or_else(|| vec![f32::NAN; fields.len()]);
    }
    set.fields = fields;
}

/// One table cell as a figure: missing or null (kept back) is NaN.
fn cell(row: &Value, key: &str, scale: f64) -> f32 {
    row.get(key)
        .and_then(Value::as_f64)
        .map_or(f32::NAN, |v| (v * scale) as f32)
}

/// SES-WOA: CBS's socio-economic status scores per buurt, wijk and gemeente,
/// the successor of SCP's *statusscores* (SCP stopped them after 2017 and CBS
/// took them over in 2022, on SCP's method). Each year recalculated to the
/// 2024 boundaries, so a buurt is the same buurt from 2014 to 2023.
const SES_TABLE: &str = "86092NED";
const SES_YEARS: std::ops::RangeInclusive<i32> = 2014..=2023;
const SES_ATTRIBUTION: &str = "Figures: CBS, Sociaal-economische status per wijk en buurt (SES-WOA, after SCP's statusscores) · areas: CBS, Kadaster, via PDOK";

/// One SES column: the table's property and the field it becomes.
struct SesColumn {
    property: &'static str,
    key: &'static str,
    label: &'static str,
    /// Also on the buurt board, whose file the series makes large: only
    /// what the buurt figure needs.
    buurt: bool,
    roll: fn() -> Roll,
}

fn by_households() -> Roll {
    Roll::Mean {
        weight: "households".into(),
    }
}

/// The score is a mean over households, so a wijk's is its buurten's weighted
/// by households, exactly; its 95 % bounds do not roll up.
const SES_COLUMNS: &[SesColumn] = &[
    SesColumn {
        property: "ParticuliereHuishoudens_2",
        key: "households",
        label: "Households",
        buurt: true,
        roll: || Roll::Sum,
    },
    SesColumn {
        property: "GemiddeldeScore_29",
        key: "ses",
        label: "Socio-economic status",
        buurt: true,
        roll: by_households,
    },
    SesColumn {
        property: "Ondergrens95Interval_28",
        key: "ses_low",
        label: "Socio-economic status, 95 % low",
        buurt: true,
        roll: || Roll::Keep,
    },
    SesColumn {
        property: "Bovengrens95Interval_30",
        key: "ses_high",
        label: "Socio-economic status, 95 % high",
        buurt: true,
        roll: || Roll::Keep,
    },
    SesColumn {
        property: "GemiddeldeScore_31",
        key: "ses_prosperity",
        label: "Prosperity part",
        buurt: false,
        roll: by_households,
    },
    SesColumn {
        property: "GemiddeldeScore_33",
        key: "ses_education",
        label: "Education part",
        buurt: false,
        roll: by_households,
    },
    SesColumn {
        property: "GemiddeldeScore_35",
        key: "ses_work",
        label: "Work part",
        buurt: false,
        roll: by_households,
    },
];

/// The SES table's rows, by region code: one value per column per year.
fn ses_values(rows: &[Value], columns: &[&SesColumn]) -> HashMap<String, Vec<f32>> {
    let years: Vec<i32> = SES_YEARS.collect();
    let mut out: HashMap<String, Vec<f32>> = Default::default();
    for row in rows {
        let (Some(code), Some(year)) =
            (region(row, "WijkenEnBuurten"), period_year(row, "Perioden"))
        else {
            continue;
        };
        let Some(yi) = years.iter().position(|&y| y == year) else {
            continue;
        };
        let v = out
            .entry(code)
            .or_insert_with(|| vec![f32::NAN; years.len() * columns.len()]);
        for (ci, c) in columns.iter().enumerate() {
            v[yi * columns.len() + ci] = cell(row, c.property, 1.0);
        }
    }
    out
}

fn ses_fields(columns: &[&SesColumn]) -> Vec<Field> {
    let keys: Vec<(&str, &str, &str, Roll)> = columns
        .iter()
        .map(|c| {
            (
                c.key,
                c.label,
                if c.key == "households" {
                    "households"
                } else {
                    ""
                },
                (c.roll)(),
            )
        })
        .collect();
    series(&keys, &SES_YEARS.collect::<Vec<_>>())
}

/// The SES sets, buurt to country.
pub fn ses_2023() -> anyhow::Result<Vec<AreaSet>> {
    let mut select = vec!["WijkenEnBuurten", "Perioden"];
    select.extend(SES_COLUMNS.iter().map(|c| c.property));
    let rows = cbs_rows(SES_TABLE, &select, "WijkenEnBuurten", &["GM", "WK", "BU"])?;
    let all: Vec<&SesColumn> = SES_COLUMNS.iter().collect();
    let buurt_only: Vec<&SesColumn> = SES_COLUMNS.iter().filter(|c| c.buurt).collect();
    let (values, buurt_values) = (ses_values(&rows, &all), ses_values(&rows, &buurt_only));

    let buurten = wfs_features(CBS_WFS, "wijkenbuurten:buurten")?;
    let wijken = wfs_features(CBS_WFS, "wijkenbuurten:wijken")?;
    let gemeenten = wfs_features(CBS_WFS, "wijkenbuurten:gemeenten")?;
    let provincies = wfs_features(BESTUURLIJK_WFS, "bestuurlijkegebieden:Provinciegebied")?;
    let land = wfs_features(BESTUURLIJK_WFS, "bestuurlijkegebieden:Landgebied")?;
    let outlines = |f: &[Value], name: &str, rung, id: &str, label: &str, parent, tol| {
        let mut s = cbs_outlines(f, name, rung, id, label, parent, tol);
        s.attribution = SES_ATTRIBUTION.into();
        s
    };
    let mut buurt = outlines(
        &buurten,
        "ses-buurt-2024",
        Rung::Neighbourhood,
        "buurtcode",
        "buurtnaam",
        Some("wijkcode"),
        0.000_1,
    );
    let mut wijk = outlines(
        &wijken,
        "ses-wijk-2024",
        Rung::District,
        "wijkcode",
        "wijknaam",
        Some("gemeentecode"),
        0.000_2,
    );
    let mut gemeente = outlines(
        &gemeenten,
        "ses-gemeente-2024",
        Rung::Municipality,
        "gemeentecode",
        "gemeentenaam",
        None,
        0.000_5,
    );
    fill(&mut buurt, ses_fields(&buurt_only), &buurt_values);
    fill(&mut wijk, ses_fields(&all), &values);
    fill(&mut gemeente, ses_fields(&all), &values);
    let (mut provincie, mut country) =
        provinces_and_country(&mut gemeente, &provincies, &land, "ses", SES_ATTRIBUTION);
    provincie.roll_up(&gemeente);
    country.roll_up(&provincie);
    Ok(vec![buurt, wijk, gemeente, provincie, country])
}

/// PBL and CBS's regional forecast of the population, 2023–2050, with its
/// 67 % interval: the last one, as the two have said they will make no more
/// (the WLO's regional scenarios follow it). On the 2021 municipalities.
const PBL_TABLE: &str = "85173NED";
const PBL_YEARS: std::ops::RangeInclusive<i32> = 2023..=2050;
const PBL_ATTRIBUTION: &str =
    "Figures: PBL/CBS, Regionale prognose 2023–2050 · areas: CBS, Kadaster, via PDOK";

/// The municipalities of 2021 that have merged since, and the 2024 one each
/// became. A forecast for one of these is added into its successor.
const MERGED_SINCE_2021: &[(&str, &str)] = &[
    ("GM0370", "GM0439"), // Beemster → Purmerend
    ("GM0398", "GM1980"), // Heerhugowaard → Dijk en Waard
    ("GM0416", "GM1980"), // Langedijk → Dijk en Waard
    ("GM0457", "GM0363"), // Weesp → Amsterdam
    ("GM0501", "GM1992"), // Brielle → Voorne aan Zee
    ("GM0530", "GM1992"), // Hellevoetsluis → Voorne aan Zee
    ("GM0614", "GM1992"), // Westvoorne → Voorne aan Zee
    ("GM0756", "GM1982"), // Boxmeer → Land van Cuijk
    ("GM0786", "GM1982"), // Grave → Land van Cuijk
    ("GM0815", "GM1982"), // Mill en Sint Hubert → Land van Cuijk
    ("GM1684", "GM1982"), // Cuijk → Land van Cuijk
    ("GM1702", "GM1982"), // Sint Anthonis → Land van Cuijk
    ("GM0856", "GM1991"), // Uden → Maashorst
    ("GM1685", "GM1991"), // Landerd → Maashorst
];

/// The forecast's rows by 2024 code: per year the population, its low and its
/// high bound. A merged municipality's population is the sum of its parts';
/// its bounds are left out, as a sum of bounds is no bound of a sum.
fn pbl_values(rows: &[Value]) -> HashMap<String, Vec<f32>> {
    let years: Vec<i32> = PBL_YEARS.collect();
    let mut parts: HashMap<String, usize> = Default::default();
    let mut out: HashMap<String, Vec<f32>> = Default::default();
    let to_2024 = |code: &str| -> String {
        if code == "NL01" {
            return "NL".into();
        }
        MERGED_SINCE_2021
            .iter()
            .find(|(old, _)| *old == code)
            .map_or(code, |(_, new)| new)
            .to_owned()
    };
    let mut seen: HashSet<(String, String)> = Default::default();
    for row in rows {
        let (Some(code), Some(year), Some(kind)) = (
            region(row, "RegioIndeling2021"),
            period_year(row, "Perioden"),
            region(row, "PrognoseInterval"),
        ) else {
            continue;
        };
        let Some(yi) = years.iter().position(|&y| y == year) else {
            continue;
        };
        let slot = match kind.as_str() {
            "MW00000" => 0,
            "MOG0067" => 1,
            "MBG0067" => 2,
            _ => continue,
        };
        let new = to_2024(&code);
        if seen.insert((code.clone(), new.clone())) {
            *parts.entry(new.clone()).or_default() += 1;
        }
        let v = out
            .entry(new)
            .or_insert_with(|| vec![f32::NAN; years.len() * 3]);
        let x = cell(row, "TotaleBevolking_1", 1000.0);
        let i = yi * 3 + slot;
        v[i] = if v[i].is_nan() { x } else { v[i] + x };
    }
    for (code, n) in parts {
        if n > 1 {
            if let Some(v) = out.get_mut(&code) {
                for yi in 0..years.len() {
                    v[yi * 3 + 1] = f32::NAN;
                    v[yi * 3 + 2] = f32::NAN;
                }
            }
        }
    }
    out
}

/// The forecast's sets: gemeente, provincie and country, each with its own
/// published figures (PBL publishes all three, so nothing is rolled up).
pub fn pbl_2023() -> anyhow::Result<Vec<AreaSet>> {
    let rows = cbs_rows(
        PBL_TABLE,
        &[
            "RegioIndeling2021",
            "PrognoseInterval",
            "Perioden",
            "TotaleBevolking_1",
        ],
        "RegioIndeling2021",
        &["NL", "PV", "GM"],
    )?;
    let values = pbl_values(&rows);
    let gemeenten = wfs_features(CBS_WFS, "wijkenbuurten:gemeenten")?;
    let provincies = wfs_features(BESTUURLIJK_WFS, "bestuurlijkegebieden:Provinciegebied")?;
    let land = wfs_features(BESTUURLIJK_WFS, "bestuurlijkegebieden:Landgebied")?;
    let mut gemeente = cbs_outlines(
        &gemeenten,
        "pbl-gemeente-2024",
        Rung::Municipality,
        "gemeentecode",
        "gemeentenaam",
        None,
        0.000_5,
    );
    gemeente.attribution = PBL_ATTRIBUTION.into();
    let missing: Vec<&str> = gemeente
        .areas
        .iter()
        .filter(|a| !values.contains_key(&a.id))
        .map(|a| a.id.as_str())
        .collect();
    if !missing.is_empty() {
        bail!("no forecast for {missing:?}: a merger since 2021 missing from MERGED_SINCE_2021");
    }
    let (mut provincie, mut country) =
        provinces_and_country(&mut gemeente, &provincies, &land, "pbl", PBL_ATTRIBUTION);
    let fields = || {
        series(
            &[
                ("people", "Inhabitants", "people", Roll::Sum),
                ("people_low", "Inhabitants, 67 % low", "people", Roll::Keep),
                (
                    "people_high",
                    "Inhabitants, 67 % high",
                    "people",
                    Roll::Keep,
                ),
            ],
            &PBL_YEARS.collect::<Vec<_>>(),
        )
    };
    for set in [&mut gemeente, &mut provincie, &mut country] {
        fill(set, fields(), &values);
    }
    Ok(vec![gemeente, provincie, country])
}

/// The WLO 2025 regional futures, High and Low, per COROP region for 2021,
/// 2040, 2050 and 2060: PBL's (with CPB) `WLO2025_PBL_Regio.xlsx`, CC BY.
pub const WLO_REGIO: &str =
    "https://dataportaal.pbl.nl/data/Welvaart_en_Leefomgeving/WLO_2025/WLO2025_PBL_Regio.xlsx";
/// The COROP regions of 2021, the division the WLO figures are on.
const GEBIEDEN_2021_WFS: &str = "https://service.pdok.nl/cbs/gebiedsindelingen/2021/wfs/v1_0";
const WLO_ATTRIBUTION: &str =
    "Figures: PBL (2025), Toekomstverkenning WLO, regional · areas: CBS, Kadaster, via PDOK";
const WLO_YEARS: [i32; 4] = [2021, 2040, 2050, 2060];
/// The two futures, as the workbook's sheets name them and as fields do.
const WLO_FUTURES: [(&str, &str); 2] = [("Hoog", "high"), ("Laag", "low")];

/// One `Demografie…` sheet by region code (`CR01`, `NL`): inhabitants,
/// households and the share aged 65 and over. The workbook counts in
/// thousands, so a small region's share is good to about a point.
fn wlo_sheet(rows: &[Vec<String>]) -> anyhow::Result<HashMap<String, [f32; 3]>> {
    let head = rows
        .iter()
        .find(|r| r.first().is_some_and(|c| c.trim() == "code"))
        .context("WLO sheet: no header row")?;
    let col = |name: &str| {
        head.iter()
            .position(|c| c.trim() == name)
            .with_context(|| format!("WLO sheet: no column {name:?}"))
    };
    let (bev, hh) = (col("bev")?, col("hh")?);
    let old = [
        col("man65_74")?,
        col("man75_EO")?,
        col("vr_65_74")?,
        col("vr_75_EO")?,
    ];
    let num = |r: &[String], i: usize| r.get(i).and_then(|c| c.trim().parse::<f64>().ok());
    let mut out = HashMap::new();
    for r in rows {
        let Some(code) = r.first().and_then(|c| c.trim().parse::<u32>().ok()) else {
            continue;
        };
        let id = if code == 2000 {
            "NL".to_owned()
        } else {
            format!("CR{code:02}")
        };
        let (Some(people), Some(households)) = (num(r, bev), num(r, hh)) else {
            continue;
        };
        let aged: f64 = old.iter().filter_map(|&i| num(r, i)).sum();
        out.insert(
            id,
            [
                (people * 1000.0) as f32,
                (households * 1000.0) as f32,
                (aged / people * 100.0) as f32,
            ],
        );
    }
    Ok(out)
}

/// The WLO figures by region: per year, per future, the three of
/// [`wlo_sheet`]. 2021 is measured, the same for both futures.
fn wlo_values(book: &crate::xlsx::Workbook) -> anyhow::Result<HashMap<String, Vec<f32>>> {
    let per_year = WLO_FUTURES.len() * 3;
    let mut out: HashMap<String, Vec<f32>> = Default::default();
    for (yi, year) in WLO_YEARS.iter().enumerate() {
        for (fi, (sheet_future, _)) in WLO_FUTURES.iter().enumerate() {
            let sheet = if *year == 2021 {
                "Demografie2021".to_owned()
            } else {
                format!("Demografie{year}{sheet_future}")
            };
            for (id, three) in wlo_sheet(&book.sheet(&sheet)?)? {
                let v = out
                    .entry(id)
                    .or_insert_with(|| vec![f32::NAN; WLO_YEARS.len() * per_year]);
                let at = yi * per_year + fi * 3;
                v[at..at + 3].copy_from_slice(&three);
            }
        }
    }
    Ok(out)
}

fn wlo_fields() -> Vec<Field> {
    let mut keys = Vec::new();
    for (_, future) in WLO_FUTURES {
        let people = format!("people_{future}");
        keys.push((people.clone(), "Inhabitants", "people", Roll::Sum));
        keys.push((
            format!("households_{future}"),
            "Households",
            "households",
            Roll::Sum,
        ));
        keys.push((
            format!("share_65_plus_{future}"),
            "Aged 65 and over",
            "%",
            Roll::Mean { weight: people },
        ));
    }
    let keys: Vec<(&str, &str, &str, Roll)> = keys
        .iter()
        .map(|(k, l, u, r)| (k.as_str(), *l, *u, r.clone()))
        .collect();
    series(&keys, &WLO_YEARS)
}

/// The WLO sets: the COROP regions and the country.
pub fn wlo_2025(from: Option<&str>) -> anyhow::Result<Vec<AreaSet>> {
    let from = from.unwrap_or(WLO_REGIO);
    let mut bytes = Vec::new();
    if from.starts_with("https://") || from.starts_with("http://") {
        ureq::get(from)
            .call()
            .with_context(|| format!("fetching {from}"))?
            .into_reader()
            .take(64 * 1024 * 1024)
            .read_to_end(&mut bytes)?;
    } else {
        bytes = std::fs::read(from).with_context(|| format!("reading {from}"))?;
    }
    let values = wlo_values(&crate::xlsx::Workbook::read(&bytes)?)?;
    let corop_features = wfs_features(
        GEBIEDEN_2021_WFS,
        "gebiedsindelingen:coropgebied_gegeneraliseerd",
    )?;
    let land = wfs_features(BESTUURLIJK_WFS, "bestuurlijkegebieden:Landgebied")?;
    let mut areas: Vec<Area> = corop_features
        .iter()
        .filter_map(|f| {
            let p = &f["properties"];
            let mut a = Area::new(
                first_text(p, &["statcode"])?,
                first_text(p, &["statnaam"])?,
                "NLD".into(),
                simplified(&polygons(&f["geometry"]), 0.001),
            );
            a.parent = Some("NL".into());
            Some(a)
        })
        .collect();
    areas.sort_by(|a, b| a.id.cmp(&b.id));
    // COROP regions sit between province and municipality; read on the
    // province rung, they reach the municipalities' heights too.
    let mut corop = AreaSet::new("wlo-corop-2021".into(), WLO_ATTRIBUTION.into(), areas)
        .with_rung(Rung::Province);
    let mut country = country_set(&land, "wlo-land-2021", WLO_ATTRIBUTION);
    if corop.areas.len() != 40 || corop.areas.iter().any(|a| !values.contains_key(&a.id)) {
        bail!("expected the 40 COROP regions of 2021, each in the WLO workbook");
    }
    fill(&mut corop, wlo_fields(), &values);
    fill(&mut country, wlo_fields(), &values);
    Ok(vec![corop, country])
}

/// Build a set by name and write its files into `out`; returns what was written.
pub fn write(
    set: &str,
    from: Option<&str>,
    out: &Path,
) -> anyhow::Result<Vec<(String, usize, usize)>> {
    let sets = match set {
        "cbs-2024" => cbs_2024()?,
        "ses-2023" => ses_2023()?,
        "pbl-2050" => pbl_2023()?,
        "wlo-2025" => wlo_2025(from)?,
        "ne-admin1-europe" => vec![natural_earth_europe(&read_source(
            from.unwrap_or(NATURAL_EARTH_ADMIN1),
        )?)?],
        "ne-admin0-europe" => vec![natural_earth_countries_europe(&read_source(
            from.unwrap_or(NATURAL_EARTH_ADMIN0),
        )?)?],
        other => bail!(
            "no area set {other:?}; there are cbs-2024, ses-2023, pbl-2050, wlo-2025, \
             ne-admin1-europe and ne-admin0-europe"
        ),
    };
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let mut written = Vec::new();
    for s in sets {
        let bytes = s.encode();
        let path = out.join(format!("{}.bin", s.name));
        // Beside, then renamed: a client fetching mid-write gets the old file.
        let tmp = path.with_extension("bin.tmp");
        std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)?;
        written.push((s.name.clone(), s.areas.len(), bytes.len()));
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_become_areas_inside_the_box() {
        let src: Value = serde_json::from_str(
            r#"{"type":"FeatureCollection","features":[
              {"type":"Feature","properties":{"adm1_code":"NLD-1","name":"Gelderland","name_en":"Gelderland","adm0_a3":"NLD"},
               "geometry":{"type":"Polygon","coordinates":[[[5,52],[6,52],[6,53],[5,53],[5,52]]]}},
              {"type":"Feature","properties":{"adm1_code":"ARG-1","name":"Entre Ríos","adm0_a3":"ARG"},
               "geometry":{"type":"MultiPolygon","coordinates":[[[[-60,-32],[-59,-32],[-59,-31],[-60,-32]]]]}}
            ]}"#,
        )
        .unwrap();
        let set = natural_earth_europe(&src).unwrap();
        assert_eq!(set.areas.len(), 1);
        assert_eq!(set.areas[0].name, "Gelderland");
        assert_eq!(
            set.areas[0].polygons[0][0].len(),
            4,
            "the closing point is dropped"
        );
        assert_eq!(set.area_at([5.5, 52.5]), Some(0));
    }

    #[test]
    fn a_merged_municipality_adds_its_forecasts_and_drops_their_bounds() {
        let row = |code: &str, kind: &str, year: i32, v: f64| {
            serde_json::json!({
                "RegioIndeling2021": format!("{code}  "),
                "PrognoseInterval": kind,
                "Perioden": format!("{year}JJ00"),
                "TotaleBevolking_1": v,
            })
        };
        let rows = vec![
            row("GM0457", "MW00000", 2050, 29.1),
            row("GM0457", "MOG0067", 2050, 25.9),
            row("GM0363", "MW00000", 2050, 1000.0),
            row("GM0363", "MOG0067", 2050, 950.0),
            row("GM0014", "MW00000", 2050, 250.0),
            row("GM0014", "MOG0067", 2050, 240.0),
            row("NL01", "MW00000", 2050, 19_000.0),
        ];
        let v = pbl_values(&rows);
        let at = |code: &str, slot: usize| v[code][(2050 - 2023) * 3 + slot];
        // Weesp went into Amsterdam in 2022.
        assert_eq!(at("GM0363", 0), 1_029_100.0);
        assert!(at("GM0363", 1).is_nan());
        assert!(!v.contains_key("GM0457"));
        assert_eq!(at("GM0014", 1), 240_000.0);
        assert_eq!(at("NL", 0), 19_000_000.0);
    }

    #[test]
    fn a_wlo_sheet_reads_by_its_header() {
        let row = |c: &[&str]| c.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let rows = vec![
            row(&["SCENARIO HOOG"]),
            row(&[
                "code ", "naam", "bev", "man65_74", "man75_EO", "vr_65_74", "vr_75_EO", "hh",
            ]),
            row(&["1", "Oost-Groningen", "134", "7", "10", "8", "11", "66"]),
            row(&[
                "2000", "totaal", "17475", "970", "635", "1005", "848", "8043",
            ]),
        ];
        let v = wlo_sheet(&rows).unwrap();
        assert_eq!(v["CR01"][0], 134_000.0);
        assert_eq!(v["CR01"][1], 66_000.0);
        assert!((v["CR01"][2] - 26.87).abs() < 0.01, "{:?}", v["CR01"]);
        assert!(v.contains_key("NL"));
        assert_eq!(wlo_fields().len(), WLO_YEARS.len() * 6);
    }

    #[test]
    fn ses_rows_become_a_series_and_null_is_kept_back() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[
              {"WijkenEnBuurten":"BU00140000","Perioden":"2014JJ00","ParticuliereHuishoudens_2":500,"GemiddeldeScore_29":-0.25,"Ondergrens95Interval_28":null},
              {"WijkenEnBuurten":"BU00140000","Perioden":"2023JJ00","ParticuliereHuishoudens_2":520,"GemiddeldeScore_29":-0.1}
            ]"#,
        )
        .unwrap();
        let cols: Vec<&SesColumn> = SES_COLUMNS.iter().filter(|c| c.buurt).collect();
        let v = &ses_values(&rows, &cols)["BU00140000"];
        let n = cols.len();
        // A negative score is a score, not one of CBS's codes.
        assert_eq!(v[1], -0.25);
        assert!(v[2].is_nan());
        assert_eq!(v[9 * n], 520.0);
        let fields = ses_fields(&cols);
        assert_eq!(fields.len(), v.len());
        assert_eq!(fields[9 * n].year, Some(2023));
    }

    #[test]
    fn a_cbs_level_keeps_land_parts_and_hides_coded_figures() {
        let features: Value = serde_json::from_str(
            r#"[
              {"type":"Feature","properties":{"buurtcode":"BU1","buurtnaam":"Binnenstad","wijkcode":"WK1","water":"NEE",
                "aantalInwoners":1200,"percentagePersonen65JaarEnOuder":23,"gemiddeldeWoningwaarde":-99997},
               "geometry":{"type":"Polygon","coordinates":[[[5,52],[5.01,52],[5.01,52.01],[5,52.01],[5,52]]]}},
              {"type":"Feature","properties":{"buurtcode":"BU1","buurtnaam":"Binnenstad","wijkcode":"WK1","water":"JA"},
               "geometry":{"type":"Polygon","coordinates":[[[5,52],[5.01,52],[5.01,52.01],[5,52]]]}}
            ]"#,
        )
        .unwrap();
        let set = cbs_level(
            features.as_array().unwrap(),
            "b",
            Rung::Neighbourhood,
            "buurtcode",
            "buurtnaam",
            Some("wijkcode"),
            0.0001,
        );
        assert_eq!(set.areas.len(), 1);
        assert_eq!(set.areas[0].parent.as_deref(), Some("WK1"));
        let f = |k: &str| set.field(k).unwrap();
        assert_eq!(set.value(0, f("people")), Some(1200.0));
        assert_eq!(set.value(0, f("share_65_plus")), Some(23.0));
        assert_eq!(set.value(0, f("home_value")), None);
        assert_eq!(set.value(0, f("households")), None);
    }
}

/// Reads back what `atlas areas` wrote, for a look by eye:
/// `CARTO_AREAS_DIR=/tmp/areas cargo test -p cartopolis_atlas written_sets -- --ignored --nocapture`.
#[cfg(test)]
#[test]
#[ignore = "reads the files a run wrote"]
fn written_sets_read_back() {
    let dir = std::env::var("CARTO_AREAS_DIR").expect("CARTO_AREAS_DIR");
    for (set, ids, key) in [
        (
            "pbl-gemeente-2024",
            &["GM0014", "GM0363", "GM1680"][..],
            "people",
        ),
        ("pbl-land-2024", &["NL"][..], "people_low"),
        ("ses-gemeente-2024", &["GM0014", "GM0363"][..], "ses"),
        ("ses-land-2024", &["NL"][..], "ses"),
        ("ses-buurt-2024", &["BU00140000"][..], "ses"),
        ("wlo-corop-2021", &["CR01", "CR17"][..], "people_low"),
        ("wlo-land-2021", &["NL"][..], "share_65_plus_high"),
    ] {
        let bytes = std::fs::read(format!("{dir}/{set}.bin")).expect(set);
        let s = AreaSet::decode(&bytes).expect(set);
        let years = s.years(key);
        for id in ids {
            let a = s.index_of(id).expect(id);
            let vals: Vec<String> = years
                .iter()
                .filter(|&&y| y % 5 == 0 || y == years[0] || Some(&y) == years.last())
                .map(|&y| {
                    let v = s.value(a, s.field_at(key, Some(y)).unwrap());
                    format!("{y}:{}", v.map_or("–".into(), |v| format!("{v:.3}")))
                })
                .collect();
            println!("{set} {id} {} {key}: {}", s.areas[a].name, vals.join(" "));
        }
    }
}
