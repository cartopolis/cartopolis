//! Re-exports canonical tile math from `cartopolis_core` (ADR-006).
#![allow(unused_imports)]
pub use cartopolis_core::world::{
    find_optimal_zoom, lat_lng_to_tile, tile_to_lat_lng, tile_to_meters, REGION_SIZE_METERS,
    REGION_ZOOM_LEVEL,
};
