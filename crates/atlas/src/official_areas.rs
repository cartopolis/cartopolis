//! **Official areas as scenario boards** (`cartopolis_scenario::board`): a
//! publisher's polygons and figures, simplified, linked to the rung above and
//! written as the compact area files the client reads layers from
//! (`docs/notes/scenarios.md`).
//!
//! Two sets so far, `atlas areas <set> --out <dir>`:
//!
//! - **`cbs-2024`**: CBS's Kerncijfers wijken en buurten 2024 from PDOK — buurt,
//!   wijk and gemeente with their figures, each linked to its parent by CBS's
//!   own codes; the provinces from PDOK's bestuurlijke gebieden, a gemeente
//!   placed in one by a point inside it; and the country. CBS publishes no
//!   province or country figures here, so those are rolled up from the
//!   gemeenten (`AreaSet::roll_up`). Five files, `cbs-<rung>-2024.bin`.
//! - **`ne-admin1-europe`**: Natural Earth's admin-1 over Europe, no figures;
//!   a province rung for scenarios beyond the Netherlands.
//!
//! These are whole-country and whole-continent files, unlike `atlas run`'s
//! per-area datasets in RD, so they are their own command. The output goes in
//! the served site tree under `areas/`, where the client fetches it.

use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context};
use cartopolis_scenario::board::{simplify_ring, Area, AreaSet, Field, LonLat, Roll};
use cartopolis_scenario::ladder::Rung;
use serde_json::Value;

/// Natural Earth's admin-1 as GeoJSON, in the project's own repository.
pub const NATURAL_EARTH_ADMIN1: &str = "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_10m_admin_1_states_provinces.geojson";

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
    let [w, s, e, n] = [-25.0, 34.0, 45.0, 72.0];
    let mut areas = Vec::new();
    for f in source["features"].as_array().into_iter().flatten() {
        let props = &f["properties"];
        let (Some(id), Some(name), Some(country)) = (
            first_text(props, &["adm1_code"]),
            first_text(props, &["name_en", "name"]),
            first_text(props, &["adm0_a3"]),
        ) else {
            continue;
        };
        let polys = simplified(&polygons(&f["geometry"]), 0.01);
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
    Ok(AreaSet::new(
        "ne-admin1-europe".into(),
        "Provinces: Natural Earth (public domain)".into(),
        areas,
    )
    .with_rung(Rung::Province))
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
        area.values = cbs_values(p);
        areas.push(area);
    }
    areas.sort_by(|a, b| a.id.cmp(&b.id));
    AreaSet::new(name.into(), CBS_ATTRIBUTION.into(), areas)
        .with_rung(rung)
        .with_fields(cbs_fields())
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
    let missed = place_in(&mut gemeente, &full);
    if missed > 0 {
        eprintln!("  {missed} gemeenten in no province");
    }
    let mut provincie = AreaSet::new(
        "cbs-provincie-2024".into(),
        CBS_ATTRIBUTION.into(),
        province_areas(Some(0.001)),
    )
    .with_rung(Rung::Province);
    provincie.roll_up(&gemeente);

    let outline: Vec<Vec<Vec<LonLat>>> = land
        .iter()
        .flat_map(|f| simplified(&polygons(&f["geometry"]), 0.002))
        .collect();
    let mut country = AreaSet::new(
        "cbs-land-2024".into(),
        CBS_ATTRIBUTION.into(),
        vec![Area::new(
            "NL".into(),
            "Nederland".into(),
            "NLD".into(),
            outline,
        )],
    )
    .with_rung(Rung::Country);
    country.roll_up(&provincie);

    Ok(vec![buurt, wijk, gemeente, provincie, country])
}

/// Build a set by name and write its files into `out`; returns what was written.
pub fn write(
    set: &str,
    from: Option<&str>,
    out: &Path,
) -> anyhow::Result<Vec<(String, usize, usize)>> {
    let sets = match set {
        "cbs-2024" => cbs_2024()?,
        "ne-admin1-europe" => vec![natural_earth_europe(&read_source(
            from.unwrap_or(NATURAL_EARTH_ADMIN1),
        )?)?],
        other => bail!("no area set {other:?}; there are cbs-2024 and ne-admin1-europe"),
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
