//! `systems::nav` — one file per concern; see systems/mod.rs for the map.

/// The group's registration: its resources and its systems, each in its
/// `crate::phase::ClientPhase`. Added by `run_client`.
mod plugin;
pub use plugin::NavPlugin;

pub mod driver;
pub mod geo_nav;
// The camera mode on the locate button: today, the sensor capture the camera
// localiser is to be developed against.
pub mod high_precision;
pub mod locate;
pub mod meetup;
pub mod navigation;
pub mod place_link;
pub mod precise_tracker;
pub mod route_ahead;
pub mod traffic;
pub mod transit_live;
pub mod transit_vehicles;
pub mod walk_guide;
