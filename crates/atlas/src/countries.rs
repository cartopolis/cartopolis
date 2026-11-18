//! The world's countries as the globe names them: one line each, with where
//! the name goes, how many live there and from how far out it is written
//! (`cartopolis_geo::countries`, `assets/globe/countries.tsv`).
//!
//! The vector tiles carry no country figures, so the source is Natural Earth's
//! admin-0 (public domain): its own label point (`LABEL_X`/`LABEL_Y`, inside
//! the land, unlike a centroid), its population estimate and its `MIN_LABEL`,
//! the web-map zoom a cartographer first writes the name at. Written once and
//! committed, as `ocean_depth` is; rerun only to take a newer Natural Earth.

use std::path::Path;

use anyhow::bail;
use serde_json::Value;

/// The file's first line, naming its columns.
pub const HEADER: &str = "a3\tname\tlon\tlat\tpopulation\tmin_label";

/// One country's line, or `None` for a feature without a name or label point.
fn line(props: &Value) -> Option<String> {
    let text = |k: &str| {
        props
            .get(k)?
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let num = |k: &str| props.get(k)?.as_f64();
    let a3 = text("ADM0_A3")?;
    let name = text("NAME_EN").or_else(|| text("NAME"))?;
    let (lon, lat) = (num("LABEL_X")?, num("LABEL_Y")?);
    // Natural Earth writes -99 where it has no estimate.
    let population = num("POP_EST").filter(|p| *p > 0.0).unwrap_or(0.0);
    let min_label = num("MIN_LABEL").filter(|z| *z >= 0.0)?;
    Some(format!(
        "{a3}\t{name}\t{lon:.4}\t{lat:.4}\t{population:.0}\t{min_label}"
    ))
}

/// The table for a Natural Earth admin-0 GeoJSON, biggest names first.
pub fn table(source: &Value) -> anyhow::Result<String> {
    let mut rows: Vec<(f64, String)> = source["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let props = &f["properties"];
            Some((props["MIN_LABEL"].as_f64()?, line(props)?))
        })
        .collect();
    if rows.is_empty() {
        bail!("no countries in the source");
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut out = String::from(HEADER);
    for (_, row) in rows {
        out.push('\n');
        out.push_str(&row);
    }
    out.push('\n');
    Ok(out)
}

/// Read Natural Earth's countries from `from` (a file or URL; Natural Earth's
/// own when `None`) and write the table to `out`.
pub fn write(from: Option<&str>, out: &Path) -> anyhow::Result<String> {
    let source = crate::official_areas::read_source(
        from.unwrap_or(crate::official_areas::NATURAL_EARTH_ADMIN0),
    )?;
    let table = table(&source)?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out, &table)?;
    Ok(format!(
        "{}: {} countries",
        out.display(),
        table.lines().count() - 1
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_country_is_one_line_and_one_without_a_label_point_is_none() {
        let src = serde_json::json!({ "features": [
            { "properties": { "ADM0_A3": "NLD", "NAME_EN": "Netherlands",
                "LABEL_X": 5.61144, "LABEL_Y": 52.422211,
                "POP_EST": 17332850.0, "MIN_LABEL": 4.0 } },
            { "properties": { "ADM0_A3": "FRA", "NAME_EN": "France",
                "LABEL_X": 2.55, "LABEL_Y": 46.69,
                "POP_EST": -99.0, "MIN_LABEL": 1.7 } },
            { "properties": { "ADM0_A3": "XXX", "NAME_EN": "Nowhere", "MIN_LABEL": 3.0 } },
        ]});
        let t = table(&src).unwrap();
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], HEADER);
        assert_eq!(lines[1], "FRA\tFrance\t2.5500\t46.6900\t0\t1.7");
        assert_eq!(lines[2], "NLD\tNetherlands\t5.6114\t52.4222\t17332850\t4");
        assert_eq!(lines.len(), 3);
    }
}
