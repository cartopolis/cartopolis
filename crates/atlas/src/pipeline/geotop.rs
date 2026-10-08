//! **GeoTOP**, the national model of the top fifty metres of the subsurface,
//! as soil columns under the street (`systems::underground`, layer `geotop`).
//!
//! TNO's model gives every 100 × 100 × 0.5 m voxel its most probable soil
//! class (`lithok`). It is served whole by TNO's OPeNDAP server, which
//! subsets by index and answers in plain text, so one request covers a city
//! (Groningen: 90 × 90 columns, ~13 MB of text) and no NetCDF reader is needed.
//!
//! * The grid's coordinates are a voxel's **lower-left corner and bottom face**
//!   (the `actual_range` attributes run one step past the last value), so a
//!   column's centre is `x + 50, y + 50` and a voxel spans `[z, z + 0.5)`.
//! * z is metres NAP. Everything above the ground is no-data (−127), so the
//!   first valid voxel from the top is the ground, and a depth is that top
//!   minus z — which is what the client wants, since it hangs every column
//!   under its own terrain.
//! * Over open water the top is the **bed**, not the surface: a lake's columns
//!   start at the water and read the water's depth too shallow. Accepted; it is
//!   ~1 % of a city's columns.
//!
//! Data: TNO / Geologische Dienst Nederland, BRO GeoTOP v1.6.1 — CC0.

use super::http::get_text_retried;
use super::rd::rd_to_lat_lng;
use super::underground::{merge_runs, Cells};
use super::{Ctx, DatasetResult, Outcome, PipelineError, Progress};
use cartopolis_geo::formats::underground::Kind;

const DATASET: &str = "https://www.dinodata.nl/opendap/GeoTOP/geotop.nc.ascii";
/// The grid's origin and spacing (`x`, `y` in RD; z in m NAP).
const X0: f64 = 13_600.0;
const Y0: f64 = 338_500.0;
const STEP: f64 = 100.0;
const NX: i64 = 2646;
const NY: i64 = 2811;
const NZ: i64 = 313;
const DZ: f32 = 0.5;
const NODATA: i32 = -127;
/// Columns are drawn this thick: a twentieth of their spacing, so a field of
/// them reads as cores in the ground rather than as a wall.
const RADIUS_M: f32 = 2.5;
/// Rows of x per request, so one request is a few megabytes of text.
const X_BATCH: i64 = 30;

const ATTRIBUTION: &str = "Soil model © TNO / Geologische Dienst Nederland, BRO GeoTOP (CC0)";

/// GeoTOP's `lithok` code as a [`Kind`]. Code 4 is retired and carries no
/// voxels.
#[must_use]
pub fn lithoclass(code: i32) -> Kind {
    match code {
        0 => Kind::Anthropogenic,
        1 => Kind::Peat,
        2 => Kind::Clay,
        3 => Kind::ClayeySand,
        5 => Kind::FineSand,
        6 => Kind::MediumSand,
        7 => Kind::CoarseSand,
        8 => Kind::Gravel,
        9 => Kind::Shells,
        _ => Kind::Unknown,
    }
}

/// One parsed column: its RD corner and the codes bottom to top.
#[derive(Debug, PartialEq)]
pub struct Column {
    pub x: f64,
    pub y: f64,
    pub codes: Vec<i32>,
}

/// Parse the ASCII answer: a `lithok.z, …` header, then one row per column,
/// `lithok.lithok[lithok.x=X][lithok.y=Y], v, v, …`, bottom to top.
pub fn parse_ascii(text: &str) -> Result<(Vec<f32>, Vec<Column>), String> {
    let mut z = Vec::new();
    let mut columns = Vec::new();
    for line in text.lines() {
        let mut parts = line.split(',').map(str::trim);
        let Some(head) = parts.next() else { continue };
        if head == "lithok.z" {
            z = parts
                .map(|v| v.parse::<f32>().map_err(|e| format!("z {v}: {e}")))
                .collect::<Result<_, _>>()?;
        } else if let Some(rest) = head.strip_prefix("lithok.lithok[lithok.x=") {
            let (x, rest) = rest.split_once("][lithok.y=").ok_or("no y in a row")?;
            let y = rest.trim_end_matches(']');
            let codes = parts
                .map(|v| v.parse::<i32>().map_err(|e| format!("code {v}: {e}")))
                .collect::<Result<Vec<_>, _>>()?;
            columns.push(Column {
                x: x.parse().map_err(|e| format!("x {x}: {e}"))?,
                y: y.parse().map_err(|e| format!("y {y}: {e}"))?,
                codes,
            });
        }
    }
    if z.is_empty() {
        return Err("no z header — not a GeoTOP answer".into());
    }
    Ok((z, columns))
}

/// A column as `(top_m, bottom_m, kind)` runs below its own top, or nothing
/// when it holds no data.
#[must_use]
pub fn column_runs(z: &[f32], codes: &[i32]) -> Vec<(f32, f32, Kind)> {
    let Some(top_i) = codes.iter().rposition(|&c| c != NODATA) else {
        return Vec::new();
    };
    let top = z[top_i] + DZ;
    let segments = (0..=top_i)
        .rev()
        .filter(|&i| codes[i] != NODATA)
        .map(|i| (top - (z[i] + DZ), top - z[i], lithoclass(codes[i])))
        .collect();
    merge_runs(segments)
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let [bx0, by0, bx1, by1] = ctx.bbox;
    let index =
        |v: f64, origin: f64, n: i64| (((v - origin) / STEP).floor() as i64).clamp(0, n - 1);
    let (xi0, xi1) = (index(bx0, X0, NX), index(bx1 - 1.0, X0, NX));
    let (yi0, yi1) = (index(by0, Y0, NY), index(by1 - 1.0, Y0, NY));
    let mut cells = Cells::default();
    let mut x = xi0;
    while x <= xi1 {
        ctx.check()?;
        let x_end = (x + X_BATCH - 1).min(xi1);
        let url = format!(
            "{DATASET}?lithok[{x}:1:{x_end}][{yi0}:1:{yi1}][0:1:{}]",
            NZ - 1
        );
        let text = get_text_retried(ctx, &url, "geotop")?;
        let (z, columns) = parse_ascii(&text).map_err(PipelineError::Failed)?;
        for c in columns {
            let runs = column_runs(&z, &c.codes);
            if runs.is_empty() {
                continue;
            }
            let (lat, lng) = rd_to_lat_lng(c.x + STEP / 2.0, c.y + STEP / 2.0);
            cells.column(lat, lng, RADIUS_M, runs);
            cells.count("soil columns");
        }
        ctx.progress(Progress {
            phase: Some("geotop".into()),
            done: Some((x_end - xi0 + 1) as u64),
            total: Some(Some((xi1 - xi0 + 1) as u64)),
        });
        x = x_end + 1;
    }
    cells.write(
        ctx,
        "geotop",
        ATTRIBUTION,
        "TNO BRO GeoTOP v1.6.1 (OPeNDAP)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = "Dataset: geotop.nc\n\
lithok.z, 0, 0.5, 1, 1.5, 2\n\
lithok.lithok[lithok.x=233900][lithok.y=582000], 2, 2, 1, 0, -127\n\
lithok.lithok[lithok.x=234000][lithok.y=582000], -127, -127, -127, -127, -127\n";

    #[test]
    fn the_ascii_answer_parses() {
        let (z, cols) = parse_ascii(ANSWER).unwrap();
        assert_eq!(z, vec![0.0, 0.5, 1.0, 1.5, 2.0]);
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].x, 233900.0);
        assert_eq!(cols[0].codes, vec![2, 2, 1, 0, -127]);
    }

    /// The top of the data is the ground: made ground half a metre thick, then
    /// peat, then a metre of clay.
    #[test]
    fn a_column_reads_downwards_from_its_own_top() {
        let (z, cols) = parse_ascii(ANSWER).unwrap();
        assert_eq!(
            column_runs(&z, &cols[0].codes),
            vec![
                (0.0, 0.5, Kind::Anthropogenic),
                (0.5, 1.0, Kind::Peat),
                (1.0, 2.0, Kind::Clay)
            ]
        );
        assert!(column_runs(&z, &cols[1].codes).is_empty());
    }

    #[test]
    fn a_non_answer_is_an_error() {
        assert!(parse_ascii("<html>Error</html>").is_err());
    }
}
