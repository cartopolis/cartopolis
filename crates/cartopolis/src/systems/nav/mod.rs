//! `systems::nav` — one file per concern; see systems/mod.rs for the map.

/// The group's registration: its resources and its systems, each in its
/// `crate::phase::ClientPhase`. Added by `run_client`.
mod plugin;
pub use plugin::NavPlugin;

pub mod driver;
pub mod geo_nav;
// An external GNSS receiver's settings, applied to `platform::gnss_receiver`.
pub mod gnss_receiver;
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
// How much of each traffic pool is out at the scene's hour.
pub mod traffic_hours;
// The traffic's Rapier bodies: what it sees ahead, and collisions.
pub mod traffic_physics;
// The walkers drawn as MakeHuman people, cut down for a crowd.
pub mod crowd;
// The walkers make way for avatars, look at them and wave back.
pub mod crowd_react;
// Cars and cyclists give way to avatars, at zebras and at a pressed button.
pub mod give_way;
// Crows over the camera and on its lawns, on the traffic's fixed-pool terms.
pub mod crows;
pub mod transit_live;
pub mod transit_vehicles;
pub mod walk_guide;
