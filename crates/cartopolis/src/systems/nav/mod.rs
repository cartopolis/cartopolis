//! `systems::nav` — one file per concern; see systems/mod.rs for the map.

/// The group's registration: its resources and its systems, each in its
/// `crate::phase::ClientPhase`. Added by `run_client`.
mod plugin;
pub use plugin::NavPlugin;

pub mod driver;
pub mod geo_nav;
pub mod locate;
pub mod navigation;
pub mod place_link;
pub mod route_ahead;
pub mod traffic;
pub mod transit_live;
pub mod transit_vehicles;
