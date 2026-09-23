//! The areas the console offers, as RD (EPSG:28992) bounding boxes, and the check
//! a typed bbox must pass. Ported from cartopy's `server/areas.ts`.
//!
//! A short built-in table rather than something an operator manages: the hard part
//! of adding a city is knowing which four numbers, in a projection nobody reads off
//! a map. These were derived from WGS84 city bounds and rounded to 500 m.
//! **Groningen's are the deployed ones, verbatim** — a refresh that quietly moved
//! its edges would drop the cells outside the new box while leaving their stale
//! files on disk.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Area {
    pub bbox: [f64; 4],
    pub id: &'static str,
    pub label: &'static str,
}

pub const AREAS: &[Area] = &[
    Area {
        bbox: [229_000.0, 578_000.0, 238_000.0, 587_000.0],
        id: "groningen",
        label: "Groningen",
    },
    Area {
        bbox: [113_500.0, 481_500.0, 130_000.0, 493_500.0],
        id: "amsterdam",
        label: "Amsterdam",
    },
    Area {
        bbox: [87_000.0, 431_500.0, 101_000.0, 442_500.0],
        id: "rotterdam",
        label: "Rotterdam",
    },
    Area {
        bbox: [75_000.0, 449_500.0, 87_500.0, 459_500.0],
        id: "den-haag",
        label: "Den Haag",
    },
    Area {
        bbox: [130_500.0, 452_500.0, 141_000.0, 460_000.0],
        id: "utrecht",
        label: "Utrecht",
    },
    Area {
        bbox: [156_000.0, 379_000.0, 165_000.0, 389_000.0],
        id: "eindhoven",
        label: "Eindhoven",
    },
    Area {
        bbox: [178_500.0, 576_000.0, 186_500.0, 582_500.0],
        id: "leeuwarden",
        label: "Leeuwarden",
    },
    Area {
        bbox: [173_500.0, 314_500.0, 181_000.0, 322_500.0],
        id: "maastricht",
        label: "Maastricht",
    },
];

#[must_use]
pub fn area(id: &str) -> Option<&'static Area> {
    AREAS.iter().find(|a| a.id == id)
}

/// Square kilometres, so the console can say how a custom box compares.
#[must_use]
pub fn area_km2(bbox: [f64; 4]) -> f64 {
    (bbox[2] - bbox[0]) * (bbox[3] - bbox[1]) / 1_000_000.0
}

/// Refuse a bbox before it becomes a twenty-minute run against somebody else's
/// server. Outside RD's useful extent there is no BAG, BGT or 3DBAG; the area
/// ceiling is a guard, not a limit of the code — at Groningen's density 400 km² is
/// half a gigabyte and an hour of somebody else's bandwidth.
pub fn validate_bbox(bbox: &[f64]) -> Result<[f64; 4], String> {
    let [xmin, ymin, xmax, ymax] = match bbox {
        [a, b, c, d] if bbox.iter().all(|v| v.is_finite()) => [*a, *b, *c, *d],
        _ => return Err("A bbox is four numbers: xmin ymin xmax ymax, in RD (EPSG:28992).".into()),
    };
    if xmax <= xmin || ymax <= ymin {
        return Err("The bbox is inside out — xmax must exceed xmin, and ymax ymin.".into());
    }
    if xmin < 0.0 || xmax > 300_000.0 || ymin < 300_000.0 || ymax > 630_000.0 {
        return Err(
            "That bbox is outside the Netherlands, where none of these registers have data.".into(),
        );
    }
    let km2 = area_km2([xmin, ymin, xmax, ymax]);
    if km2 > 400.0 {
        return Err(format!(
            "That is {} km² — over the 400 km² ceiling. Run it in pieces.",
            km2.round()
        ));
    }
    Ok([xmin, ymin, xmax, ymax])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bbox_is_checked_before_it_costs_anyone_anything() {
        assert!(validate_bbox(&[1.0, 2.0, 3.0]).is_err());
        assert!(validate_bbox(&[238_000.0, 578_000.0, 229_000.0, 587_000.0])
            .unwrap_err()
            .contains("inside out"));
        assert!(validate_bbox(&[400_000.0, 578_000.0, 410_000.0, 587_000.0])
            .unwrap_err()
            .contains("outside"));
        assert!(validate_bbox(&[100_000.0, 400_000.0, 130_000.0, 430_000.0])
            .unwrap_err()
            .contains("900 km²"));
        assert_eq!(validate_bbox(&AREAS[0].bbox).unwrap(), AREAS[0].bbox);
    }
}
