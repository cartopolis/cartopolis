//! The slippy-tile cell every extractor bins into.
//!
//! `CACHE_ZOOM` is `cartopolis_geo::formats::CACHE_ZOOM`, the one the client's
//! streamers read too, and the formula is `cartopolis_core::world`'s — a cell
//! numbered differently here is a cell the client fetches from an address nothing
//! was written to, which shows up as a 404 per tile and an empty map, not as an
//! error. Sharing both is what makes that impossible now.

pub use cartopolis_geo::formats::CACHE_ZOOM;

pub type Cell = (i64, i64);

#[must_use]
pub fn lat_lng_to_cell(lat: f64, lng: f64) -> Cell {
    cartopolis_core::lat_lng_to_tile(lat, lng, CACHE_ZOOM)
}

/// The cell list a coverage manifest carries: sorted by x, then y, numerically,
/// as `[x, y]` pairs.
#[must_use]
pub fn sorted_cells(cells: impl IntoIterator<Item = Cell>) -> Vec<[i64; 2]> {
    let mut out: Vec<[i64; 2]> = cells.into_iter().map(|(x, y)| [x, y]).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// `"<dataset>/14/{x}/{y}.json"`, or without a prefix for the root tree.
#[must_use]
pub fn cell_path(prefix: &str, cell: Cell) -> String {
    let (x, y) = cell;
    if prefix.is_empty() {
        format!("{CACHE_ZOOM}/{x}/{y}.json")
    } else {
        format!("{prefix}/{CACHE_ZOOM}/{x}/{y}.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_sort_numerically_not_lexically() {
        assert_eq!(
            sorted_cells([(10, 1), (9, 1), (9, 0), (10, 1)]),
            vec![[9, 0], [9, 1], [10, 1]]
        );
    }

    #[test]
    fn the_grote_markt_is_the_client_s_cell() {
        assert_eq!(lat_lng_to_cell(53.2194, 6.5665), (8490, 5320));
        assert_eq!(cell_path("bag", (8490, 5320)), "bag/14/8490/5320.json");
    }
}
