//! The tall-structure lane's cell and its Overpass query — the one statement of
//! both that the client (`systems::tall_structures`) and atlas's `tall` dataset
//! (`crates/atlas/src/pipeline/tall.rs`) share.
//!
//! Atlas pre-cuts exactly what the client would otherwise ask Overpass for, cell by
//! cell, and the client reads the file in its place. That only holds while the two
//! agree on the zoom, the ring the client streams and the query text, so they live
//! here rather than as two copies with a comment asking the next person to keep
//! them in step.

use cartopolis_core::world::tile_to_lat_lng;

/// Fetch zoom: a z11 cell is ~12 km across at Dutch latitudes.
pub const ZOOM: u32 = 11;

/// Cells out from the camera's cell the client streams (2 → 5×5 ≈ ±30 km). Atlas
/// grows its area by the same number, so a client anywhere inside the area finds
/// its whole ring pre-cut.
pub const RING: i64 = 2;

/// The Overpass `bbox` of one z11 cell, `south,west,north,east`.
#[must_use]
pub fn bbox(cx: i64, cy: i64) -> String {
    let (north, west) = tile_to_lat_lng(cx, cy, ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, ZOOM);
    format!("{south},{west},{north},{east}")
}

/// Everything tall in one cell, answered `out center` so a turbine mapped as an
/// area way still costs a single coordinate.
#[must_use]
pub fn query(cx: i64, cy: i64) -> String {
    format!(
        concat!(
            "[out:json][timeout:25];(",
            "node[\"man_made\"~\"^(windmill|water_tower|chimney|mast|tower|communications_tower|crane)$\"]({bb});",
            "way[\"man_made\"~\"^(windmill|water_tower|chimney|mast|tower|communications_tower|crane)$\"]({bb});",
            "node[\"power\"=\"generator\"][\"generator:source\"=\"wind\"]({bb});",
            "way[\"power\"=\"generator\"][\"generator:source\"=\"wind\"]({bb});",
            ");out center;"
        ),
        bb = bbox(cx, cy)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_names_its_cell_s_box_four_times() {
        let q = query(1061, 664);
        let bb = bbox(1061, 664);
        assert_eq!(q.matches(&bb).count(), 4);
        assert!(q.starts_with("[out:json]") && q.ends_with("out center;"));
        let parts: Vec<f64> = bb.split(',').map(|s| s.parse().unwrap()).collect();
        assert!(parts[0] < parts[2] && parts[1] < parts[3], "{bb}");
    }
}
