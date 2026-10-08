//! **Heat and cold storage** in the ground — the wells of open systems and
//! the loops of closed ones — as columns to their depth (`systems::underground`,
//! layer `wko`).
//!
//! The BRO's groundwater usage register (GUF) is the one open source that says
//! how deep they go; it has no WFS, only a national GeoPackage refreshed daily
//! (~57 MB), which is downloaded once per run and read with SQLite. The schema
//! is relational rather than simple features: a position is RD text in
//! `bro_point`, reached through `bro_location`, which carries one foreign key
//! per kind of object. A well counts only when its facility is licensed as an
//! **open ground-energy system** — the same register holds drinking-water and
//! dewatering wells. A loop is always a closed ground-energy system.
//!
//! The screens migrated from the old LGR register are untidy (`top=65,
//! length=0` beside `top=0, length=75`), so a well is drawn as a tube to the
//! deepest of its screen and its stated depth, with the screen marked where it
//! is a real interval. Coverage is still filling as the LGR migrates: 114 loops
//! here against 865 in RVO's own map.
//!
//! Data: BRO Grondwatergebruiksysteem (GUF) via PDOK — CC0.

use rusqlite::{Connection, OpenFlags};

use super::http::download_to;
use super::rd::rd_to_lat_lng;
use super::underground::Cells;
use super::{Ctx, DatasetResult, Outcome, PipelineError};
use cartopolis_geo::formats::underground::Kind;

const GPKG: &str =
    "https://service.pdok.nl/tno/bro-grondwatergebruiksysteem/atom/downloads/brogufvolledigeset.gpkg";
const WELL_RADIUS_M: f32 = 0.35;
const LOOP_RADIUS_M: f32 = 0.2;
const ATTRIBUTION: &str = "Heat storage wells © BRO Grondwatergebruiksysteem (TNO) via PDOK — CC0";

/// A well's column: a tube to its screen in the well's temperature colour, the
/// screen itself, and on to the stated depth.
#[must_use]
pub fn well_column(
    temperature: Option<&str>,
    depth: Option<f64>,
    screen_top: Option<f64>,
    screen_length: Option<f64>,
) -> Vec<(f32, f32, Kind)> {
    let tube = match temperature.map(str::trim) {
        Some("warm") => Kind::HeatWarm,
        Some("koud") => Kind::HeatCold,
        _ => Kind::WellTube,
    };
    let top = screen_top.filter(|t| *t >= 0.0).unwrap_or(0.0) as f32;
    let bottom = top + screen_length.filter(|l| *l > 0.0).unwrap_or(0.0) as f32;
    let end = (depth.unwrap_or(0.0) as f32).max(bottom);
    let mut out = Vec::new();
    if top > 0.0 {
        out.push((0.0, top, tube));
    }
    if bottom > top {
        out.push((top, bottom, Kind::WellFilter));
    }
    if end > bottom.max(top) {
        out.push((bottom.max(top), end, tube));
    }
    out
}

fn inside(bbox: [f64; 4], x: f64, y: f64) -> bool {
    x >= bbox[0] && x < bbox[2] && y >= bbox[1] && y < bbox[3]
}

fn sql_err(e: rusqlite::Error) -> PipelineError {
    PipelineError::Failed(format!("guf: {e}"))
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let dir = ctx
        .data_dir()
        .map(|d| d.join("source-cache"))
        .unwrap_or_else(|_| std::env::temp_dir());
    std::fs::create_dir_all(&dir).map_err(|e| PipelineError::Failed(e.to_string()))?;
    let path = dir.join("brogufvolledigeset.gpkg");
    let bytes = download_to(ctx, GPKG, &path, "guf")?;
    ctx.log(format!("GUF: {:.0} MB", bytes as f64 / 1e6));
    let db =
        Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql_err)?;
    let bbox = ctx.bbox;
    let mut cells = Cells::default();

    // Wells of open ground-energy systems, each with its first screen.
    let mut wells = db
        .prepare(
            "SELECT p.x_or_lon, p.y_or_lat, w.relative_temperature, w.well_depth,
                    (SELECT s.top_screen_depth FROM realised_screen s
                      WHERE s.realised_well_fk = w.realised_well_pk ORDER BY s.realised_screen_pk LIMIT 1),
                    (SELECT s.length FROM realised_screen s
                      WHERE s.realised_well_fk = w.realised_well_pk ORDER BY s.realised_screen_pk LIMIT 1)
               FROM realised_well w
               JOIN bro_location l ON l.realised_well_fk = w.realised_well_pk AND l.crs = 'RD'
               JOIN bro_point p ON p.bro_location_fk = l.bro_location_pk
               JOIN realised_installation i ON i.realised_installation_pk = w.realised_installation_fk
              WHERE EXISTS (SELECT 1 FROM licence_groundwater_usage g
                              JOIN usage_type_facility u ON u.licence_groundwater_usage_fk = g.licence_groundwater_usage_pk
                             WHERE g.groundwater_usage_facility_fk = i.groundwater_usage_facility_fk
                               AND u.primary_usage_type = 'openBodemenergiesysteem')",
        )
        .map_err(sql_err)?;
    let rows = wells
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<f64>>(3)?,
                r.get::<_, Option<f64>>(4)?,
                r.get::<_, Option<f64>>(5)?,
            ))
        })
        .map_err(sql_err)?;
    for row in rows {
        let (x, y, temperature, depth, top, length) = row.map_err(sql_err)?;
        let (Ok(x), Ok(y)) = (x.trim().parse::<f64>(), y.trim().parse::<f64>()) else {
            continue;
        };
        if !inside(bbox, x, y) {
            continue;
        }
        let column = well_column(temperature.as_deref(), depth, top, length);
        if column.is_empty() {
            continue;
        }
        let (lat, lng) = rd_to_lat_lng(x, y);
        cells.column(lat, lng, WELL_RADIUS_M, column);
        cells.count("open system wells");
    }

    // Closed loops, to their depth.
    let mut loops = db
        .prepare(
            "SELECT p.x_or_lon, p.y_or_lat, lp.loop_depth
               FROM realised_loop lp
               JOIN bro_location l ON l.realised_loop_fk = lp.realised_loop_pk AND l.crs = 'RD'
               JOIN bro_point p ON p.bro_location_fk = l.bro_location_pk",
        )
        .map_err(sql_err)?;
    let rows = loops
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<f64>>(2)?,
            ))
        })
        .map_err(sql_err)?;
    for row in rows {
        let (x, y, depth) = row.map_err(sql_err)?;
        let (Ok(x), Ok(y), Some(depth)) = (x.trim().parse::<f64>(), y.trim().parse::<f64>(), depth)
        else {
            continue;
        };
        if !inside(bbox, x, y) || depth <= 0.0 {
            continue;
        }
        let (lat, lng) = rd_to_lat_lng(x, y);
        cells.column(
            lat,
            lng,
            LOOP_RADIUS_M,
            vec![(0.0, depth as f32, Kind::HeatLoop)],
        );
        cells.count("closed loops");
    }

    cells.write(ctx, "wko", ATTRIBUTION, "BRO GUF (PDOK GeoPackage)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_is_a_tube_a_screen_and_the_rest() {
        assert_eq!(
            well_column(Some("koud"), Some(90.0), Some(40.0), Some(21.0)),
            vec![
                (0.0, 40.0, Kind::HeatCold),
                (40.0, 61.0, Kind::WellFilter),
                (61.0, 90.0, Kind::HeatCold)
            ]
        );
    }

    /// The untidy migrated screens still give a sensible column.
    #[test]
    fn untidy_screens_still_draw() {
        assert_eq!(
            well_column(Some("warm"), None, Some(0.0), Some(75.0)),
            vec![(0.0, 75.0, Kind::WellFilter)]
        );
        assert_eq!(
            well_column(None, None, Some(65.0), Some(0.0)),
            vec![(0.0, 65.0, Kind::WellTube)]
        );
        assert!(well_column(None, None, None, None).is_empty());
    }
}
