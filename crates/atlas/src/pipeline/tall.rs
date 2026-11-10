//! The **tall structures** — wind turbines, windmills, masts, towers, chimneys,
//! water towers and cranes — pre-cut into the z11 cells the client's tall lane
//! streams (`systems::tall_structures` reads them).
//!
//! The client used to ask Overpass for every one of those cells itself, and on a
//! cold start that made this lane the last thing to finish: timed on the Steam
//! Deck at 600 m over Groningen (2026-09-26) it landed at 24.3 s, ~8 s after
//! everything else, because each of its 25 cells was a live query costing
//! 0.7–2.6 s of server time. Everything else the client streams is a static file
//! this crate wrote; now this is too.
//!
//! **The file is the Overpass answer itself**, for the same query the client
//! would have sent (`cartopolis_geo::tall::query`): an `out center` document,
//! nodes with `lat`/`lon`, ways with a `center`. Since 2026-11 it is cut from the
//! OpenStreetMap extract (`crate::osm`, `osm::layers::tall`) rather than fetched
//! from Overpass, so building it needs no Overpass server.
//!
//! The area is grown by the client's ring ([`tall::RING`]) so that a client
//! standing anywhere inside it finds its whole ring pre-cut, not just the cell
//! under it.
//!
//! Data: © OpenStreetMap contributors, ODbL.

use cartopolis_core::world::lat_lng_to_tile;
use cartopolis_geo::tall;

use super::rd::wgs84_bbox;
use super::tile::Cell;
use super::{Ctx, DatasetResult, Outcome};

const ATTRIBUTION: &str = "Tall structures © OpenStreetMap contributors (ODbL)";

/// RD metres the area is padded by before it is binned: the client picks its
/// centre cell from a *rounded* base-zoom tile index, which can land one z14
/// tile (~1.5 km) past where the camera actually stands.
const EDGE_PAD_M: f64 = 1_500.0;

/// Every z11 cell a client standing anywhere in `bbox_rd` asks for: the cells
/// the (padded) area touches, grown by the client's ring on every side, in row
/// order.
#[must_use]
pub fn area_cells(bbox_rd: [f64; 4]) -> Vec<Cell> {
    let [xmin, ymin, xmax, ymax] = bbox_rd;
    let [west, south, east, north] = wgs84_bbox([
        xmin - EDGE_PAD_M,
        ymin - EDGE_PAD_M,
        xmax + EDGE_PAD_M,
        ymax + EDGE_PAD_M,
    ]);
    let (x0, y0) = lat_lng_to_tile(north, west, tall::ZOOM);
    let (x1, y1) = lat_lng_to_tile(south, east, tall::ZOOM);
    let mut cells = Vec::new();
    for y in y0 - tall::RING..=y1 + tall::RING {
        for x in x0 - tall::RING..=x1 + tall::RING {
            cells.push((x, y));
        }
    }
    cells
}

/// `tall/11/{x}/{y}.json` — where the client looks.
#[must_use]
pub fn cell_path((x, y): Cell) -> String {
    format!("tall/{}/{x}/{y}.json", tall::ZOOM)
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let cells = area_cells(ctx.bbox);
    crate::osm::build_cells(
        ctx,
        "tall",
        tall::ZOOM,
        &cells,
        &crate::osm::layers::tall::WANTED,
        crate::osm::layers::tall::cell,
        ATTRIBUTION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;
    const GRONINGEN: [f64; 4] = [229_000.0, 578_000.0, 238_000.0, 587_000.0];

    /// **Every cell of every ring a client inside the area can stand at is
    /// covered** — the corners' cells, grown by the client's ring on each side.
    #[test]
    fn the_area_is_grown_by_the_client_s_ring() {
        let cells: HashSet<Cell> = area_cells(GRONINGEN).into_iter().collect();
        let [xmin, ymin, xmax, ymax] = GRONINGEN;
        for (x, y) in [(xmin, ymin), (xmax, ymin), (xmin, ymax), (xmax, ymax)] {
            let (lat, lng) = super::super::rd::rd_to_lat_lng(x, y);
            let (cx, cy) = lat_lng_to_tile(lat, lng, tall::ZOOM);
            for dy in -tall::RING..=tall::RING {
                for dx in -tall::RING..=tall::RING {
                    assert!(
                        cells.contains(&(cx + dx, cy + dy)),
                        "{:?}",
                        (cx + dx, cy + dy)
                    );
                }
            }
        }
        // The Grote Markt's own ring, and not a continent: Groningen spans one or
        // three z11 cells a side once padded, so the grown block is five to seven, and
        // it is a block — no holes, no duplicates.
        let markt = lat_lng_to_tile(53.2194, 6.5665, tall::ZOOM);
        assert!(cells.contains(&markt));
        let (xs, ys): (Vec<i64>, Vec<i64>) = cells.iter().copied().unzip();
        let w = xs.iter().max().unwrap() - xs.iter().min().unwrap() + 1;
        let h = ys.iter().max().unwrap() - ys.iter().min().unwrap() + 1;
        assert!((5..=7).contains(&w) && (5..=7).contains(&h), "{w}×{h}");
        assert_eq!(cells.len() as i64, w * h);
        assert_eq!(area_cells(GRONINGEN).len(), cells.len());
    }
}
